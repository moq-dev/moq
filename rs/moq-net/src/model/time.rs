use rand::Rng;

use crate::coding::{Decode, DecodeError, Encode, EncodeError, VarInt};

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Returned when a [`Timestamp`] operation would exceed the QUIC VarInt range
/// (`2^62 - 1`), overflow during scale conversion or arithmetic, hit a divide
/// by zero from an unspecified ([`Timescale::UNKNOWN`]) scale, or attempt
/// arithmetic between timestamps with mismatched scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// Units per second used by a track for frame timestamps.
///
/// Newtype around `u64`. Use the named constants ([`Self::SECOND`], [`Self::MILLI`],
/// [`Self::MICRO`], [`Self::NANO`]) instead of writing raw integers at call sites.
///
/// [`Self::UNKNOWN`] (raw value `0`) denotes an unspecified scale, produced by
/// [`Timestamp::ZERO`] and [`Timestamp::MAX`] sentinels. Unit conversions against
/// an unknown scale return [`TimeOverflow`] to avoid divide by zero.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timescale(pub u64);

impl Timescale {
	/// Unspecified scale. Conversions involving this return [`TimeOverflow`].
	pub const UNKNOWN: Self = Self(0);
	/// One unit per second (`1`).
	pub const SECOND: Self = Self(1);
	/// 1,000 units per second (`1_000`).
	pub const MILLI: Self = Self(1_000);
	/// 1,000,000 units per second (`1_000_000`). Common default for media tracks.
	pub const MICRO: Self = Self(1_000_000);
	/// 1,000,000,000 units per second (`1_000_000_000`).
	pub const NANO: Self = Self(1_000_000_000);

	/// Construct a timescale from a raw value (units per second). `0` means [`Self::UNKNOWN`].
	pub const fn new(units_per_second: u64) -> Self {
		Self(units_per_second)
	}

	/// The raw units-per-second value.
	pub const fn as_u64(self) -> u64 {
		self.0
	}

	/// Whether this is [`Self::UNKNOWN`] (raw value `0`).
	pub const fn is_unknown(self) -> bool {
		self.0 == 0
	}
}

impl From<u64> for Timescale {
	fn from(units_per_second: u64) -> Self {
		Self(units_per_second)
	}
}

impl From<Timescale> for u64 {
	fn from(scale: Timescale) -> Self {
		scale.0
	}
}

impl std::fmt::Debug for Timescale {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match *self {
			Self::UNKNOWN => write!(f, "Timescale::UNKNOWN"),
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
/// All timestamps within a track are relative, so zero for one track is not zero for another.
/// The underlying value is constrained to fit within a QUIC VarInt (`2^62 - 1`) so it can be
/// encoded and decoded easily; the scale is carried alongside so frames from different
/// sources can be compared and converted without lossy detours through a single fixed scale.
///
/// [`Timescale::UNKNOWN`] denotes an unspecified timescale, produced by [`Timestamp::ZERO`]
/// and [`Timestamp::MAX`]. Unit conversions and arithmetic against an unknown scale return
/// [`TimeOverflow`] to avoid divide by zero.
#[derive(Clone, Default, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timestamp {
	value: VarInt,
	scale: Timescale,
}

impl Timestamp {
	/// A zero timestamp with an unspecified scale.
	///
	/// Useful as a sentinel min: comparisons against unspecified-scale timestamps
	/// compare raw values, so `Timestamp::ZERO < t` is true for any `t` whose
	/// `value > 0`. See [`Self::cmp`].
	pub const ZERO: Self = Self {
		value: VarInt::ZERO,
		scale: Timescale::UNKNOWN,
	};

	/// The maximum representable timestamp value, with an unspecified scale.
	///
	/// Useful as a sentinel max: `t < Timestamp::MAX` is true for any `t` whose
	/// `value < VarInt::MAX`. See [`Self::cmp`].
	pub const MAX: Self = Self {
		value: VarInt::MAX,
		scale: Timescale::UNKNOWN,
	};

	/// Construct a timestamp directly from a raw value at the given scale.
	pub const fn new(value: u64, scale: Timescale) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(value) {
			Some(value) => Ok(Self { value, scale }),
			None => Err(TimeOverflow),
		}
	}

	/// Convert `value` measured at `source` (units per second) to a timestamp at `target`.
	///
	/// Returns [`TimeOverflow`] on overflow or if `source` is [`Timescale::UNKNOWN`].
	pub const fn from_scale(value: u64, source: Timescale, target: Timescale) -> Result<Self, TimeOverflow> {
		if source.0 == 0 {
			return Err(TimeOverflow);
		}
		match (value as u128).checked_mul(target.0 as u128) {
			Some(scaled) => match VarInt::from_u128(scaled / source.0 as u128) {
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

	/// The scale (units per second) attached to this timestamp.
	pub const fn scale(self) -> Timescale {
		self.scale
	}

	/// Whether the scale is [`Timescale::UNKNOWN`]. Unit conversions and cross-scale
	/// arithmetic against this timestamp return [`TimeOverflow`].
	pub const fn is_unspecified(self) -> bool {
		self.scale.0 == 0
	}

	/// Whether the raw value is zero. Does not consider scale.
	pub const fn is_zero(self) -> bool {
		self.value.into_inner() == 0
	}

	/// Re-express this timestamp at a new scale. Returns [`TimeOverflow`] if the new
	/// value would exceed `2^62 - 1`, the source scale is unspecified, or `new_scale`
	/// is [`Timescale::UNKNOWN`].
	pub const fn convert(self, new_scale: Timescale) -> Result<Self, TimeOverflow> {
		if self.scale.0 == 0 || new_scale.0 == 0 {
			return Err(TimeOverflow);
		}
		if self.scale.0 == new_scale.0 {
			return Ok(self);
		}
		Self::from_scale(self.value.into_inner(), self.scale, new_scale)
	}

	/// The value re-expressed at `target` as a `u128`. Returns [`TimeOverflow`]
	/// if the source scale is unspecified or `target` is [`Timescale::UNKNOWN`].
	pub const fn as_scale(self, target: Timescale) -> Result<u128, TimeOverflow> {
		if self.scale.0 == 0 || target.0 == 0 {
			return Err(TimeOverflow);
		}
		Ok(self.value.into_inner() as u128 * target.0 as u128 / self.scale.0 as u128)
	}

	/// The value re-expressed in seconds. Returns [`TimeOverflow`] if the scale is
	/// unspecified.
	pub const fn as_secs(self) -> Result<u64, TimeOverflow> {
		if self.scale.0 == 0 {
			return Err(TimeOverflow);
		}
		Ok(self.value.into_inner() / self.scale.0)
	}

	/// The value re-expressed in milliseconds. Returns [`TimeOverflow`] if the scale
	/// is unspecified.
	pub const fn as_millis(self) -> Result<u128, TimeOverflow> {
		self.as_scale(Timescale::MILLI)
	}

	/// The value re-expressed in microseconds. Returns [`TimeOverflow`] if the scale
	/// is unspecified.
	pub const fn as_micros(self) -> Result<u128, TimeOverflow> {
		self.as_scale(Timescale::MICRO)
	}

	/// The value re-expressed in nanoseconds. Returns [`TimeOverflow`] if the scale
	/// is unspecified.
	pub const fn as_nanos(self) -> Result<u128, TimeOverflow> {
		self.as_scale(Timescale::NANO)
	}

	/// Return the larger of two timestamps.
	///
	/// Panics if the scales differ. Use [`Self::convert`] first if you need to compare
	/// across scales.
	pub const fn max(self, other: Self) -> Self {
		assert!(self.scale.0 == other.scale.0, "mismatched timestamp scales");
		if self.value.into_inner() > other.value.into_inner() {
			self
		} else {
			other
		}
	}

	/// Add two timestamps. Returns [`TimeOverflow`] if the sum exceeds `2^62 - 1`,
	/// if either scale is [`Timescale::UNKNOWN`], or if the scales differ.
	pub const fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale.0 == 0 || rhs.scale.0 == 0 || self.scale.0 != rhs.scale.0 {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_add(rhs.value.into_inner()) {
			Some(result) => Self::new(result, self.scale),
			None => Err(TimeOverflow),
		}
	}

	/// Subtract `rhs` from `self`. Returns [`TimeOverflow`] if `rhs > self`, if either
	/// scale is [`Timescale::UNKNOWN`], or if the scales differ.
	pub const fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale.0 == 0 || rhs.scale.0 == 0 || self.scale.0 != rhs.scale.0 {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_sub(rhs.value.into_inner()) {
			Some(result) => Self::new(result, self.scale),
			None => Err(TimeOverflow),
		}
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
		if self.scale.0 == 0 {
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
	/// Compare two timestamps, normalizing across scales when both are known.
	///
	/// - If both scales are equal, compares raw values directly.
	/// - If one side is [`Timescale::UNKNOWN`] ([`Self::ZERO`] / [`Self::MAX`] sentinels),
	///   compares raw values; the sentinel's `0` / `VarInt::MAX` ensures the expected
	///   ordering against any concrete timestamp.
	/// - If both scales are known but differ, cross-multiplies in 128-bit so e.g.
	///   `1s > 2ms` orders correctly. Required by the `min_by_key` call sites in the
	///   fmp4/mkv exporters, which pick the next track to emit across mixed-scale
	///   per-track frames.
	///
	/// When the cross-scale comparison would otherwise be `Equal` (e.g. `from_secs(1)`
	/// vs `from_millis(1000)`), we break ties by `(scale, value)` so the result agrees
	/// with derived `PartialEq`/`Eq`/`Hash` (which are field-wise). Without that tie
	/// break, `cmp` could return `Equal` for fields that aren't equal, violating
	/// Rust's `Ord`/`Eq` contract for ordered collection keys.
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		if self.scale.0 == other.scale.0 || self.scale.0 == 0 || other.scale.0 == 0 {
			return self.value.cmp(&other.value);
		}
		let lhs = self.value.into_inner() as u128 * other.scale.0 as u128;
		let rhs = other.value.into_inner() as u128 * self.scale.0 as u128;
		lhs.cmp(&rhs)
			.then_with(|| self.scale.0.cmp(&other.scale.0))
			.then_with(|| self.value.cmp(&other.value))
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

/// Decode a timestamp's raw value as a varint, attaching [`Timescale::UNKNOWN`].
///
/// Callers that need a meaningful scale should attach it after decoding (e.g. via
/// [`Timestamp::new`] with the track's [`Timescale`]).
impl Decode<crate::Version> for Timestamp {
	fn decode<R: bytes::Buf>(r: &mut R, version: crate::Version) -> Result<Self, DecodeError> {
		let value = VarInt::decode(r, version)?;
		Ok(Self {
			value,
			scale: Timescale::UNKNOWN,
		})
	}
}

/// Encode a timestamp's raw value as a varint. The scale is **not** serialized.
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
		assert_eq!(time.scale(), Timescale::SECOND);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_millis().unwrap(), 5000);
		assert_eq!(time.as_micros().unwrap(), 5_000_000);
		assert_eq!(time.as_nanos().unwrap(), 5_000_000_000);
	}

	#[test]
	fn test_from_millis() {
		let time = Timestamp::from_millis(5000).unwrap();
		assert_eq!(time.scale(), Timescale::MILLI);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_millis().unwrap(), 5000);
	}

	#[test]
	fn test_from_micros() {
		let time = Timestamp::from_micros(5_000_000).unwrap();
		assert_eq!(time.scale(), Timescale::MICRO);
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_micros().unwrap(), 5_000_000);
	}

	#[test]
	fn test_from_nanos() {
		let time = Timestamp::from_nanos(5_000_000_000).unwrap();
		assert_eq!(time.scale(), Timescale::NANO);
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
		let time_us = time_ms.convert(Timescale::MICRO).unwrap();
		assert_eq!(time_us.scale(), Timescale::MICRO);
		assert_eq!(time_us.as_micros().unwrap(), 5_000_000);
	}

	#[test]
	fn test_convert_to_coarser() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_s = time_ms.convert(Timescale::SECOND).unwrap();
		assert_eq!(time_s.scale(), Timescale::SECOND);
		assert_eq!(time_s.as_secs().unwrap(), 5);
	}

	#[test]
	fn test_convert_precision_loss() {
		// 1234 ms = 1.234 s, rounds down to 1 s
		let time_ms = Timestamp::from_millis(1234).unwrap();
		let time_s = time_ms.convert(Timescale::SECOND).unwrap();
		assert_eq!(time_s.as_secs().unwrap(), 1);
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
	fn test_convert_unspecified_rejected() {
		let zero = Timestamp::ZERO;
		assert!(zero.convert(Timescale::MILLI).is_err());

		let time = Timestamp::from_millis(5).unwrap();
		assert!(time.convert(Timescale::UNKNOWN).is_err());
	}

	#[test]
	fn test_add_same_scale() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_millis(2000).unwrap();
		let c = a.checked_add(b).unwrap();
		assert_eq!(c.as_millis().unwrap(), 3000);
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
	fn test_add_unspecified_scale_rejected() {
		// Two ZERO sentinels (both UNKNOWN scale) must not combine into a meaningful
		// result, otherwise wire-decoded timestamps that haven't had a scale attached
		// yet would silently behave like real arithmetic operands.
		assert!(Timestamp::ZERO.checked_add(Timestamp::ZERO).is_err());

		let t = Timestamp::from_millis(100).unwrap();
		assert!(t.checked_add(Timestamp::ZERO).is_err());
		assert!(Timestamp::ZERO.checked_add(t).is_err());
	}

	#[test]
	fn test_sub_unspecified_scale_rejected() {
		assert!(Timestamp::ZERO.checked_sub(Timestamp::ZERO).is_err());

		let t = Timestamp::from_millis(100).unwrap();
		assert!(t.checked_sub(Timestamp::ZERO).is_err());
		assert!(Timestamp::ZERO.checked_sub(t).is_err());
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
	fn test_ordering_across_known_scales() {
		// Cross-scale ordering must normalize to a common scale rather than fall back
		// to raw values. Without this, 1s would order as < 2ms (1 < 2) and the
		// fmp4/mkv exporter's `min_by_key(|ts| *ts)` over per-track frames would silently
		// pick the wrong track when tracks have different native scales.
		let one_sec = Timestamp::from_secs(1).unwrap();
		let two_ms = Timestamp::from_millis(2).unwrap();
		assert!(one_sec > two_ms);
		assert!(two_ms < one_sec);

		// Temporally-equivalent timestamps with different (value, scale) representations
		// must NOT cmp as Equal: derived Eq compares fields, so cmp returning Equal
		// here would violate the Ord/Eq contract for ordered collection keys. The
		// tie-breaker resolves them by scale to keep ordering deterministic.
		let one_sec_b = Timestamp::from_millis(1000).unwrap();
		assert_ne!(one_sec.cmp(&one_sec_b), std::cmp::Ordering::Equal);
		assert_ne!(one_sec, one_sec_b);
		// And the tie-break agrees with PartialEq: cmp(a, b) == Equal iff a == b.
		assert_eq!(one_sec.cmp(&one_sec), std::cmp::Ordering::Equal);

		// Sorting a mixed-scale slice puts entries in correct temporal order.
		let mut items = [
			Timestamp::from_secs(2).unwrap(),
			Timestamp::from_millis(500).unwrap(),
			Timestamp::from_micros(1_500_000).unwrap(),
		];
		items.sort();
		assert_eq!(items[0], Timestamp::from_millis(500).unwrap());
		assert_eq!(items[1], Timestamp::from_micros(1_500_000).unwrap());
		assert_eq!(items[2], Timestamp::from_secs(2).unwrap());
	}

	#[test]
	fn test_duration_conversion() {
		let duration = std::time::Duration::from_secs(5);
		let time: Timestamp = duration.try_into().unwrap();
		assert_eq!(time.scale(), Timescale::NANO);
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
		let t = Timestamp::new(5000, Timescale::MILLI).unwrap();
		assert_eq!(t.value(), 5000);
		assert_eq!(t.scale(), Timescale::MILLI);
		assert_eq!(t.as_millis().unwrap(), 5000);
	}

	#[test]
	fn test_from_scale_custom() {
		// 120 units at 60Hz = 2 seconds, expressed at 1000Hz = 2000 ms.
		let t = Timestamp::from_scale(120, Timescale::new(60), Timescale::MILLI).unwrap();
		assert_eq!(t.scale(), Timescale::MILLI);
		assert_eq!(t.as_millis().unwrap(), 2000);
	}

	#[test]
	fn test_from_scale_zero_source() {
		assert!(Timestamp::from_scale(5, Timescale::UNKNOWN, Timescale::MILLI).is_err());
	}
}
