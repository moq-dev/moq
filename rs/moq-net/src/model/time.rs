use rand::Rng;

use crate::Error;
use crate::coding::{Decode, DecodeError, Encode, EncodeError, VarInt};

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Returned when a [`Timestamp`] operation would exceed the QUIC VarInt range
/// (`2^62 - 1`), overflow during scale conversion or arithmetic, hit a divide
/// by zero from an unspecified ([`Timestamp::is_unspecified`]) scale, or
/// attempt arithmetic between timestamps with mismatched scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// A timestamp in a track's timescale (units per second).
///
/// All timestamps within a track are relative, so zero for one track is not zero for another.
/// The underlying value is constrained to fit within a QUIC VarInt (`2^62 - 1`) so it can be
/// encoded and decoded easily; the scale is carried out-of-band (via [`crate::Track::timescale`])
/// and not serialized per-timestamp.
///
/// `scale == 0` denotes an unspecified timescale, produced by [`Timestamp::ZERO`] and by
/// peers that don't negotiate a timescale (older moq-lite versions, older moq-transport
/// drafts without track properties). Unit conversions and arithmetic against an unspecified
/// scale return [`TimeOverflow`] to avoid divide by zero.
#[derive(Clone, Default, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timestamp {
	value: VarInt,
	scale: u64,
}

impl Timestamp {
	/// A zero timestamp with an unspecified scale.
	///
	/// Useful as a sentinel min: comparisons against unspecified-scale timestamps
	/// compare raw values, so `Timestamp::ZERO < t` is true for any `t` whose
	/// `value > 0`. See [`Self::partial_cmp`].
	pub const ZERO: Self = Self {
		value: VarInt::ZERO,
		scale: 0,
	};

	/// The maximum representable timestamp value, with an unspecified scale.
	///
	/// Useful as a sentinel max: `t < Timestamp::MAX` is true for any `t` whose
	/// `value < VarInt::MAX`. See [`Self::partial_cmp`].
	pub const MAX: Self = Self {
		value: VarInt::MAX,
		scale: 0,
	};

	/// Construct a timestamp directly from a raw value at the given scale.
	pub const fn new(value: u32, scale: u64) -> Self {
		Self {
			value: VarInt::from_u32(value),
			scale,
		}
	}

	/// Construct a timestamp from a raw value at the given scale. Returns [`TimeOverflow`]
	/// if `value` exceeds the 62-bit varint range.
	pub const fn new_u64(value: u64, scale: u64) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(value) {
			Some(value) => Ok(Self { value, scale }),
			None => Err(TimeOverflow),
		}
	}

	/// Convert `value` measured at the given `scale` (units per second) to a timestamp.
	///
	/// The stored scale is `target_scale`. Returns [`TimeOverflow`] on overflow or if
	/// `source_scale` is zero.
	pub const fn from_scale(value: u64, source_scale: u64, target_scale: u64) -> Result<Self, TimeOverflow> {
		if source_scale == 0 {
			return Err(TimeOverflow);
		}
		match (value as u128).checked_mul(target_scale as u128) {
			Some(scaled) => match VarInt::from_u128(scaled / source_scale as u128) {
				Some(value) => Ok(Self {
					value,
					scale: target_scale,
				}),
				None => Err(TimeOverflow),
			},
			None => Err(TimeOverflow),
		}
	}

	/// Convert a number of seconds to a timestamp with `scale == 1`.
	pub const fn from_secs(seconds: u64) -> Result<Self, TimeOverflow> {
		Self::new_u64(seconds, 1)
	}

	/// Like [`Self::from_secs`] but panics on overflow.
	pub const fn from_secs_unchecked(seconds: u64) -> Self {
		match Self::from_secs(seconds) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of milliseconds to a timestamp with `scale == 1_000`.
	pub const fn from_millis(millis: u64) -> Result<Self, TimeOverflow> {
		Self::new_u64(millis, 1_000)
	}

	/// Like [`Self::from_millis`] but panics on overflow.
	pub const fn from_millis_unchecked(millis: u64) -> Self {
		match Self::from_millis(millis) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of microseconds to a timestamp with `scale == 1_000_000`.
	pub const fn from_micros(micros: u64) -> Result<Self, TimeOverflow> {
		Self::new_u64(micros, 1_000_000)
	}

	/// Like [`Self::from_micros`] but panics on overflow.
	pub const fn from_micros_unchecked(micros: u64) -> Self {
		match Self::from_micros(micros) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of nanoseconds to a timestamp with `scale == 1_000_000_000`.
	pub const fn from_nanos(nanos: u64) -> Result<Self, TimeOverflow> {
		Self::new_u64(nanos, 1_000_000_000)
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

	/// The scale (units per second) attached to this timestamp.
	pub const fn scale(self) -> u64 {
		self.scale
	}

	/// Whether the scale is unset (`scale == 0`). Unit conversions and cross-scale
	/// arithmetic against this timestamp return [`TimeOverflow`].
	pub const fn is_unspecified(self) -> bool {
		self.scale == 0
	}

	/// Whether the raw value is zero. Does not consider scale.
	pub const fn is_zero(self) -> bool {
		self.value.into_inner() == 0
	}

	/// Re-express this timestamp at a new scale. Returns [`TimeOverflow`] if the new
	/// value would exceed `2^62 - 1`, the source scale is unspecified, or
	/// `new_scale == 0`.
	pub const fn convert(self, new_scale: u64) -> Result<Self, TimeOverflow> {
		if self.scale == 0 || new_scale == 0 {
			return Err(TimeOverflow);
		}
		if self.scale == new_scale {
			return Ok(self);
		}
		Self::from_scale(self.value.into_inner(), self.scale, new_scale)
	}

	/// The value re-expressed at `target_scale` as a `u128`. Returns [`TimeOverflow`]
	/// if the source scale is unspecified or `target_scale == 0`.
	pub const fn as_scale(self, target_scale: u64) -> Result<u128, TimeOverflow> {
		if self.scale == 0 || target_scale == 0 {
			return Err(TimeOverflow);
		}
		Ok(self.value.into_inner() as u128 * target_scale as u128 / self.scale as u128)
	}

	/// The value re-expressed in seconds. Returns [`TimeOverflow`] if the scale is
	/// unspecified.
	pub const fn as_secs(self) -> Result<u64, TimeOverflow> {
		if self.scale == 0 {
			return Err(TimeOverflow);
		}
		Ok(self.value.into_inner() / self.scale)
	}

	/// The value re-expressed in milliseconds. Returns [`TimeOverflow`] if the scale
	/// is unspecified.
	pub const fn as_millis(self) -> Result<u128, TimeOverflow> {
		self.as_scale(1_000)
	}

	/// The value re-expressed in microseconds. Returns [`TimeOverflow`] if the scale
	/// is unspecified.
	pub const fn as_micros(self) -> Result<u128, TimeOverflow> {
		self.as_scale(1_000_000)
	}

	/// The value re-expressed in nanoseconds. Returns [`TimeOverflow`] if the scale
	/// is unspecified.
	pub const fn as_nanos(self) -> Result<u128, TimeOverflow> {
		self.as_scale(1_000_000_000)
	}

	/// Return the larger of two timestamps.
	///
	/// Panics if the scales differ. Use [`Self::convert`] first if you need to compare
	/// across scales.
	pub const fn max(self, other: Self) -> Self {
		assert!(self.scale == other.scale, "mismatched timestamp scales");
		if self.value.into_inner() > other.value.into_inner() {
			self
		} else {
			other
		}
	}

	/// Add two timestamps. Returns [`TimeOverflow`] if the sum exceeds `2^62 - 1` or
	/// if the scales differ.
	pub const fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale != rhs.scale {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_add(rhs.value.into_inner()) {
			Some(result) => Self::new_u64(result, self.scale),
			None => Err(TimeOverflow),
		}
	}

	/// Subtract `rhs` from `self`. Returns [`TimeOverflow`] if `rhs > self` or if the
	/// scales differ.
	pub const fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale != rhs.scale {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_sub(rhs.value.into_inner()) {
			Some(result) => Self::new_u64(result, self.scale),
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
			Some(value) => Ok(Self { value, scale: self.scale }),
			None => Err(TimeOverflow),
		}
	}

	/// The signed delta from `prev` to `self` in their shared scale. Returns
	/// [`TimeOverflow`] on scale mismatch or if the delta is outside `i64::MIN..=i64::MAX`.
	pub const fn checked_delta_from(self, prev: Self) -> Result<i64, TimeOverflow> {
		if self.scale != prev.scale {
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

	/// Current time, expressed in microseconds (`scale == 1_000_000`). Uses
	/// [`tokio::time::Instant::now`] so it honors `tokio::time::pause` in tests.
	pub fn now() -> Self {
		tokio::time::Instant::now().into()
	}

	/// Encode the raw value as a QUIC varint. Scale is carried out-of-band and is
	/// **not** included on the wire.
	pub fn encode_value<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
		self.value.encode(w, crate::lite::Version::Lite01)?;
		Ok(())
	}

	/// Decode a raw value as a QUIC varint, attaching the given scale.
	pub fn decode_value<R: bytes::Buf>(r: &mut R, scale: u64) -> Result<Self, Error> {
		let value = VarInt::decode(r, crate::lite::Version::Lite01)?;
		Ok(Self { value, scale })
	}
}

impl TryFrom<std::time::Duration> for Timestamp {
	type Error = TimeOverflow;

	/// Convert a [`std::time::Duration`] into a nanosecond-scale timestamp.
	fn try_from(duration: std::time::Duration) -> Result<Self, Self::Error> {
		match VarInt::from_u128(duration.as_nanos()) {
			Some(value) => Ok(Self {
				value,
				scale: 1_000_000_000,
			}),
			None => Err(TimeOverflow),
		}
	}
}

impl TryFrom<Timestamp> for std::time::Duration {
	type Error = TimeOverflow;

	fn try_from(time: Timestamp) -> Result<Self, Self::Error> {
		let secs = time.as_secs()?;
		let nanos = time.as_nanos()?;
		Ok(std::time::Duration::new(secs, (nanos % 1_000_000_000) as u32))
	}
}

impl std::fmt::Debug for Timestamp {
	#[allow(clippy::manual_is_multiple_of)] // is_multiple_of is unstable in Rust 1.85
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if self.scale == 0 {
			return write!(f, "{}/?", self.value.into_inner());
		}

		let nanos = match self.as_nanos() {
			Ok(n) => n,
			Err(_) => return write!(f, "{}/{}", self.value.into_inner(), self.scale),
		};

		// Choose the largest unit where we don't need decimal places.
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
	/// Compare by raw value. Debug-asserts that scales are compatible (same, or
	/// one is unspecified). In release, cross-scale comparisons return a result
	/// based on the raw value, which is meaningful only when one side is a
	/// scale-`0` sentinel ([`Self::ZERO`] or [`Self::MAX`]).
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		debug_assert!(
			self.scale == other.scale || self.scale == 0 || other.scale == 0,
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
	/// jittered wall-clock reference (see [`TIME_ANCHOR`]).
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

/// Decode a timestamp's raw value as a varint, attaching an unspecified scale.
///
/// Callers that need a meaningful scale (the track timescale) should use
/// [`Timestamp::decode_value`] directly.
impl Decode<crate::Version> for Timestamp {
	fn decode<R: bytes::Buf>(r: &mut R, version: crate::Version) -> Result<Self, DecodeError> {
		let value = VarInt::decode(r, version)?;
		Ok(Self { value, scale: 0 })
	}
}

/// Encode a timestamp's raw value as a varint. The scale is **not** serialized; it
/// is conveyed out-of-band via [`crate::Track::timescale`].
impl Encode<crate::Version> for Timestamp {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: crate::Version) -> Result<(), EncodeError> {
		self.value.encode(w, version)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_from_secs() {
		let time = Timestamp::from_secs(5).unwrap();
		assert_eq!(time.scale(), 1);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_millis().unwrap(), 5000);
		assert_eq!(time.as_micros().unwrap(), 5_000_000);
		assert_eq!(time.as_nanos().unwrap(), 5_000_000_000);
	}

	#[test]
	fn test_from_millis() {
		let time = Timestamp::from_millis(5000).unwrap();
		assert_eq!(time.scale(), 1_000);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_millis().unwrap(), 5000);
	}

	#[test]
	fn test_from_micros() {
		let time = Timestamp::from_micros(5_000_000).unwrap();
		assert_eq!(time.scale(), 1_000_000);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_micros().unwrap(), 5_000_000);
	}

	#[test]
	fn test_from_nanos() {
		let time = Timestamp::from_nanos(5_000_000_000).unwrap();
		assert_eq!(time.scale(), 1_000_000_000);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_nanos().unwrap(), 5_000_000_000);
	}

	#[test]
	fn test_zero_unspecified() {
		let time = Timestamp::ZERO;
		assert!(time.is_unspecified());
		assert!(time.is_zero());
		assert!(time.as_secs().is_err());
		assert!(time.as_millis().is_err());
		assert!(time.as_micros().is_err());
		assert!(time.as_nanos().is_err());
	}

	#[test]
	fn test_zero_at_scale() {
		let time = Timestamp::from_millis(0).unwrap();
		assert!(!time.is_unspecified());
		assert!(time.is_zero());
		assert_eq!(time.as_millis().unwrap(), 0);
	}

	#[test]
	fn test_convert_to_finer() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_us = time_ms.convert(1_000_000).unwrap();
		assert_eq!(time_us.scale(), 1_000_000);
		assert_eq!(time_us.as_micros().unwrap(), 5_000_000);
	}

	#[test]
	fn test_convert_to_coarser() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_s = time_ms.convert(1).unwrap();
		assert_eq!(time_s.scale(), 1);
		assert_eq!(time_s.as_secs().unwrap(), 5);
	}

	#[test]
	fn test_convert_precision_loss() {
		// 1234 ms = 1.234 s, rounds down to 1 s
		let time_ms = Timestamp::from_millis(1234).unwrap();
		let time_s = time_ms.convert(1).unwrap();
		assert_eq!(time_s.as_secs().unwrap(), 1);
	}

	#[test]
	fn test_convert_roundtrip() {
		let original = Timestamp::from_millis(5000).unwrap();
		let as_micros = original.convert(1_000_000).unwrap();
		let back = as_micros.convert(1_000).unwrap();
		assert_eq!(original.value(), back.value());
		assert_eq!(original.scale(), back.scale());
	}

	#[test]
	fn test_convert_same_scale() {
		let time = Timestamp::from_millis(5000).unwrap();
		let converted = time.convert(1_000).unwrap();
		assert_eq!(time, converted);
	}

	#[test]
	fn test_convert_unspecified_rejected() {
		let zero = Timestamp::ZERO;
		assert!(zero.convert(1_000).is_err());

		let time = Timestamp::from_millis(5).unwrap();
		assert!(time.convert(0).is_err());
	}

	#[test]
	fn test_add_same_scale() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_millis(2000).unwrap();
		let c = a.checked_add(b).unwrap();
		assert_eq!(c.as_millis().unwrap(), 3000);
		assert_eq!(c.scale(), 1_000);
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
	fn test_ordering_against_sentinels() {
		// ZERO and MAX act as universal sentinels because their scale is 0.
		let t = Timestamp::from_millis(100).unwrap();
		assert!(Timestamp::ZERO < t);
		assert!(t < Timestamp::MAX);
		assert!(Timestamp::ZERO < Timestamp::MAX);
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
		assert_eq!(next.as_millis().unwrap(), 150);
	}

	#[test]
	fn test_add_delta_negative() {
		let t = Timestamp::from_millis(150).unwrap();
		let next = t.checked_add_delta(-50).unwrap();
		assert_eq!(next.as_millis().unwrap(), 100);
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
		assert_eq!(time.scale(), 1_000_000_000);
		assert_eq!(time.as_secs().unwrap(), 5);

		let duration_back: std::time::Duration = time.try_into().unwrap();
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

		let t = Timestamp::ZERO;
		assert_eq!(format!("{:?}", t), "0/?");
	}

	#[test]
	fn test_new() {
		let t = Timestamp::new(5000, 1_000);
		assert_eq!(t.value(), 5000);
		assert_eq!(t.scale(), 1_000);
		assert_eq!(t.as_millis().unwrap(), 5000);
	}

	#[test]
	fn test_from_scale_custom() {
		// 120 units at 60Hz = 2 seconds, expressed at 1000Hz = 2000 ms.
		let t = Timestamp::from_scale(120, 60, 1_000).unwrap();
		assert_eq!(t.scale(), 1_000);
		assert_eq!(t.as_millis().unwrap(), 2000);
	}

	#[test]
	fn test_from_scale_zero_source() {
		assert!(Timestamp::from_scale(5, 0, 1_000).is_err());
	}
}
