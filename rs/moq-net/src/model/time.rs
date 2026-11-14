use std::num::NonZero;

use crate::coding::varint::MAX_QUIC;

/// The largest [`Timestamp`] value, `2^61 - 1`.
///
/// lite-05/06 zigzag a group's first timestamp from zero into a QUIC varint, which halves
/// its positive range. Bounding every timestamp here keeps each one encodable on every
/// version, including after a relay forwards it from lite-07 or moq-transport.
const MAX_TIMESTAMP: u64 = (1 << 61) - 1;

/// `value`, or `None` past `max`.
const fn bounded(value: u128, max: u64) -> Option<u64> {
	if value <= max as u128 { Some(value as u64) } else { None }
}

/// Returned when a [`Timestamp`] operation would exceed its range (`2^61 - 1`), a
/// [`Timescale`] would exceed the QUIC varint range (`2^62 - 1`), scale conversion or
/// arithmetic would overflow, arithmetic mixes timestamps with mismatched scales, or a
/// conversion needs the scale an unscaled [`Timestamp`] lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// Units per second used by a track for frame timestamps.
///
/// Newtype around [`NonZero<u64>`]. Zero is structurally impossible, so the
/// arithmetic on [`Timestamp`] can divide by `self.scale` without ever risking
/// a divide by zero. Use the named constants ([`Self::SECOND`], [`Self::MILLI`],
/// [`Self::MICRO`], [`Self::NANO`]) instead of writing raw integers at call sites;
/// for runtime values, use [`Self::new`] or [`TryFrom`], which return [`TimeOverflow`]
/// for `0` or for values past the QUIC varint range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timescale(NonZero<u64>);

impl Timescale {
	/// One unit per second (`1`).
	pub const SECOND: Self = match Self::new(1) {
		Ok(scale) => scale,
		Err(_) => unreachable!(),
	};
	/// 1,000 units per second (`1_000`).
	pub const MILLI: Self = match Self::new(1_000) {
		Ok(scale) => scale,
		Err(_) => unreachable!(),
	};
	/// 1,000,000 units per second (`1_000_000`). Widely used by container formats.
	pub const MICRO: Self = match Self::new(1_000_000) {
		Ok(scale) => scale,
		Err(_) => unreachable!(),
	};
	/// 1,000,000,000 units per second (`1_000_000_000`).
	pub const NANO: Self = match Self::new(1_000_000_000) {
		Ok(scale) => scale,
		Err(_) => unreachable!(),
	};

	/// Construct a timescale from a raw value (units per second).
	///
	/// Returns [`TimeOverflow`] if `units_per_second` is `0` (would divide by zero)
	/// or exceeds `2^62 - 1` (the QUIC varint range, matching [`Timestamp`] values).
	/// Every runtime constructor, including [`TryFrom`], goes through this check.
	pub const fn new(units_per_second: u64) -> Result<Self, TimeOverflow> {
		// Reject values that wouldn't fit in a QUIC varint, keeping the constraint
		// symmetric with Timestamp's raw value.
		if bounded(units_per_second as u128, MAX_QUIC).is_none() {
			return Err(TimeOverflow);
		}
		match NonZero::new(units_per_second) {
			Some(n) => Ok(Self(n)),
			None => Err(TimeOverflow),
		}
	}

	/// The raw units-per-second value (always non-zero).
	pub const fn as_u64(self) -> u64 {
		self.0.get()
	}
}

impl TryFrom<u64> for Timescale {
	type Error = TimeOverflow;

	fn try_from(units_per_second: u64) -> Result<Self, Self::Error> {
		Self::new(units_per_second)
	}
}

impl TryFrom<NonZero<u64>> for Timescale {
	type Error = TimeOverflow;

	/// Same bound as [`Self::new`]: non-zero is not enough, the value must also fit
	/// in a QUIC varint.
	fn try_from(units_per_second: NonZero<u64>) -> Result<Self, Self::Error> {
		Self::new(units_per_second.get())
	}
}

impl From<Timescale> for u64 {
	fn from(scale: Timescale) -> Self {
		scale.0.get()
	}
}

impl From<Timescale> for NonZero<u64> {
	fn from(scale: Timescale) -> Self {
		scale.0
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

/// A timestamp in a track's timescale (units per second), or a scale-less track's group key.
///
/// All timestamps within a track are relative, so zero for one track is not zero for another.
/// A scaled value is at most `2^61 - 1`, the most every moq-lite version can encode as a
/// group's first frame, while an unscaled key spans every group key; the scale is carried alongside so frames from different
/// sources can be compared and converted without lossy detours through a single fixed scale.
///
/// A scale-less track has no time: each frame's timestamp is [`Self::unscaled`], whose value
/// is its group's key. Anything that needs a scale ([`Self::convert`], `as_secs`, `as_millis`,
/// and so on) is [`TimeOverflow`] on one, and it orders against no scaled timestamp.
///
/// # An instant, not a number
///
/// A `Timestamp` is a point in time (like [`std::time::Instant`]), not a scalar, so it has no
/// arithmetic operators: adding two instants is meaningless, and a scale mismatch can't be a
/// silent panic. Use [`Self::checked_add`] / [`Self::checked_sub`], which **require both
/// operands to share a scale** and return [`TimeOverflow`] otherwise. To combine timestamps
/// from different scales, [`Self::convert`] one to the other's scale first.
///
/// # Equality vs ordering
///
/// These two intentionally disagree, so pick the one you mean:
///
/// - [`Eq`] / [`Hash`] are **structural** (field-wise): `from_secs(1) != from_millis(1000)`,
///   because they encode as different `(value, scale)` pairs on the wire. Two timestamps are
///   equal only when both their value and scale match.
/// - [`PartialOrd`] is **temporal**: it cross-multiplies scales, so `from_millis(1000)` orders
///   after `from_millis(999)` and `from_secs(1)` slots in between. When a cross-scale comparison
///   is otherwise a tie, it breaks by `(scale, value)` to stay consistent with `Eq`. A scaled
///   and an unscaled timestamp don't compare: `partial_cmp` is `None`, so `<` and `>` are both
///   false. There is no [`Ord`].
///
/// So `from_secs(1).partial_cmp(&from_millis(1000))` is *not* `Equal`, and neither is `==` true.
/// If you want "same instant regardless of encoding", compare after a [`Self::convert`] to a
/// common scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
	value: u64,
	scale: Option<Timescale>,
}

impl Timestamp {
	/// The zero timestamp: value `0` at [`Timescale::SECOND`].
	///
	/// The scale is not incidental. Equality and ordering are scale-aware (see the type
	/// docs), so this is *not* interchangeable with `0` at another scale; use
	/// [`Self::is_zero`] to test a zero value regardless of scale. In particular, don't
	/// seed a `.max()` accumulator with this: a later value at a finer scale would lose
	/// the tie-break. Reach for `Option<Timestamp>` instead.
	pub const ZERO: Self = Self::new_const(0, Timescale::SECOND);

	/// Construct a timestamp directly from a raw value at the given scale.
	/// Returns [`TimeOverflow`] if `value` exceeds `2^61 - 1`.
	pub const fn new(value: u64, scale: Timescale) -> Result<Self, TimeOverflow> {
		Self::bounded(value, Some(scale))
	}

	/// A scale-less track's timestamp: `key` is its group's key, with no time.
	pub const fn unscaled(key: u64) -> Self {
		Self {
			value: key,
			scale: None,
		}
	}

	/// `value` at `scale`, bounded to `2^61 - 1` when it carries a time.
	const fn bounded(value: u64, scale: Option<Timescale>) -> Result<Self, TimeOverflow> {
		match (scale, bounded(value as u128, MAX_TIMESTAMP)) {
			(None, _) => Ok(Self { value, scale }),
			(Some(_), Some(value)) => Ok(Self { value, scale }),
			(Some(_), None) => Err(TimeOverflow),
		}
	}

	/// Const-context twin of [`Self::new`] that panics on overflow.
	const fn new_const(value: u64, scale: Timescale) -> Self {
		match Self::new(value, scale) {
			Ok(time) => time,
			Err(_) => panic!("timestamp value exceeds 2^61 - 1"),
		}
	}

	/// Construct a timestamp from a raw value and a `units_per_second` scale.
	/// Returns [`TimeOverflow`] if the scale is zero or the value is out of range.
	pub fn from_scale(value: u64, units_per_second: u64) -> Result<Self, TimeOverflow> {
		Self::new(value, Timescale::new(units_per_second)?)
	}

	/// Convert a number of seconds to a timestamp at [`Timescale::SECOND`].
	pub const fn from_secs(seconds: u64) -> Result<Self, TimeOverflow> {
		Self::new(seconds, Timescale::SECOND)
	}

	/// Convert a number of milliseconds to a timestamp at [`Timescale::MILLI`].
	pub const fn from_millis(millis: u64) -> Result<Self, TimeOverflow> {
		Self::new(millis, Timescale::MILLI)
	}

	/// Convert a number of microseconds to a timestamp at [`Timescale::MICRO`].
	pub const fn from_micros(micros: u64) -> Result<Self, TimeOverflow> {
		Self::new(micros, Timescale::MICRO)
	}

	/// Convert a number of nanoseconds to a timestamp at [`Timescale::NANO`].
	pub const fn from_nanos(nanos: u64) -> Result<Self, TimeOverflow> {
		Self::new(nanos, Timescale::NANO)
	}

	/// The raw value in the timestamp's own scale, or the group key when unscaled.
	pub const fn value(self) -> u64 {
		self.value
	}

	/// The scale (units per second) attached to this timestamp, or `None` when unscaled.
	pub const fn scale(self) -> Option<Timescale> {
		self.scale
	}

	/// Whether the raw value is zero. Does not consider scale.
	pub const fn is_zero(self) -> bool {
		self.value == 0
	}

	/// The scale, or [`TimeOverflow`] when unscaled: a group key has no time to convert.
	const fn scaled(self) -> Result<NonZero<u64>, TimeOverflow> {
		match self.scale {
			Some(scale) => Ok(scale.0),
			None => Err(TimeOverflow),
		}
	}

	/// Re-express this timestamp at a new scale. Returns [`TimeOverflow`] if it is unscaled,
	/// or if the new value would exceed `2^61 - 1`.
	pub const fn convert(self, new_scale: Timescale) -> Result<Self, TimeOverflow> {
		let scale = match self.scaled() {
			Ok(scale) => scale.get(),
			Err(err) => return Err(err),
		};
		if scale == new_scale.0.get() {
			return Ok(self);
		}
		match (self.value as u128).checked_mul(new_scale.0.get() as u128) {
			Some(scaled) => match bounded(scaled / scale as u128, MAX_TIMESTAMP) {
				Some(value) => Ok(Self {
					value,
					scale: Some(new_scale),
				}),
				None => Err(TimeOverflow),
			},
			None => Err(TimeOverflow),
		}
	}

	/// The value re-expressed at `target` as a `u128`, or [`TimeOverflow`] when unscaled.
	pub const fn as_scale(self, target: Timescale) -> Result<u128, TimeOverflow> {
		match self.scaled() {
			Ok(scale) => Ok(self.value as u128 * target.0.get() as u128 / scale.get() as u128),
			Err(err) => Err(err),
		}
	}

	/// The value re-expressed in seconds, or [`TimeOverflow`] when unscaled.
	pub const fn as_secs(self) -> Result<u64, TimeOverflow> {
		match self.scaled() {
			Ok(scale) => Ok(self.value / scale.get()),
			Err(err) => Err(err),
		}
	}

	/// The value re-expressed in milliseconds, or [`TimeOverflow`] when unscaled.
	pub const fn as_millis(self) -> Result<u128, TimeOverflow> {
		self.as_scale(Timescale::MILLI)
	}

	/// The value re-expressed in microseconds, or [`TimeOverflow`] when unscaled.
	pub const fn as_micros(self) -> Result<u128, TimeOverflow> {
		self.as_scale(Timescale::MICRO)
	}

	/// The value re-expressed in nanoseconds, or [`TimeOverflow`] when unscaled.
	pub const fn as_nanos(self) -> Result<u128, TimeOverflow> {
		self.as_scale(Timescale::NANO)
	}

	/// Whether both carry the same scale, or both none.
	const fn same_scale(self, rhs: Self) -> bool {
		match (self.scale, rhs.scale) {
			(Some(a), Some(b)) => a.0.get() == b.0.get(),
			(None, None) => true,
			_ => false,
		}
	}

	/// Add two timestamps. Returns [`TimeOverflow`] if the sum exceeds `2^61 - 1` or
	/// if the scales differ.
	pub const fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if !self.same_scale(rhs) {
			return Err(TimeOverflow);
		}
		match self.value.checked_add(rhs.value) {
			Some(result) => Self::bounded(result, self.scale),
			None => Err(TimeOverflow),
		}
	}

	/// Subtract `rhs` from `self`. Returns [`TimeOverflow`] if `rhs > self` or if the
	/// scales differ.
	pub const fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if !self.same_scale(rhs) {
			return Err(TimeOverflow);
		}
		match self.value.checked_sub(rhs.value) {
			Some(result) => Self::bounded(result, self.scale),
			None => Err(TimeOverflow),
		}
	}
}

impl Timestamp {
	/// This timestamp when it carries a time, or `None` for a scale-less track's key.
	pub(crate) const fn timed(self) -> Option<Self> {
		match self.scale {
			Some(_) => Some(self),
			None => None,
		}
	}

	/// A received frame's time: the `timestamp` its wire carried, or on a scale-less track,
	/// which carries none, its group's `key`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn received(timestamp: Option<Self>, key: u64) -> Self {
		match timestamp {
			Some(timestamp) => timestamp,
			None => Self::unscaled(key),
		}
	}

	/// The later of two timestamps of one track, which share a scale; `self` when they
	/// don't compare.
	pub(crate) fn later(self, other: Self) -> Self {
		if other > self { other } else { self }
	}

	/// The earlier of two timestamps of one track, which share a scale; `self` when they
	/// don't compare.
	pub(crate) fn earlier(self, other: Self) -> Self {
		if other < self { other } else { self }
	}
}

impl TryFrom<std::time::Duration> for Timestamp {
	type Error = TimeOverflow;

	/// Convert a [`std::time::Duration`] into a nanosecond-scale timestamp.
	fn try_from(duration: std::time::Duration) -> Result<Self, Self::Error> {
		match bounded(duration.as_nanos(), MAX_TIMESTAMP) {
			Some(value) => Ok(Self {
				value,
				scale: Some(Timescale::NANO),
			}),
			None => Err(TimeOverflow),
		}
	}
}

impl TryFrom<Timestamp> for std::time::Duration {
	type Error = TimeOverflow;

	/// The time since the track's zero, or [`TimeOverflow`] when unscaled.
	fn try_from(time: Timestamp) -> Result<Self, Self::Error> {
		let nanos = time.as_nanos()?;
		Ok(std::time::Duration::new(
			time.as_secs()?,
			(nanos % 1_000_000_000) as u32,
		))
	}
}

impl std::fmt::Debug for Timestamp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let Ok(nanos) = self.as_nanos() else {
			return write!(f, "#{}", self.value);
		};

		// Choose the largest unit where we don't need decimal places.
		if nanos.is_multiple_of(1_000_000_000) {
			write!(f, "{}s", nanos / 1_000_000_000)
		} else if nanos.is_multiple_of(1_000_000) {
			write!(f, "{}ms", nanos / 1_000_000)
		} else if nanos.is_multiple_of(1_000) {
			write!(f, "{}µs", nanos / 1_000)
		} else {
			write!(f, "{}ns", nanos)
		}
	}
}

impl PartialOrd for Timestamp {
	/// Temporal comparison, normalizing across scales (see the type-level docs for how
	/// this relates to structural `Eq`).
	///
	/// - Equal scales, or two unscaled keys, compare raw values directly.
	/// - Otherwise cross-multiplies in 128-bit so e.g. `1s > 2ms` orders correctly.
	/// - A would-be cross-scale tie (e.g. `from_secs(1)` vs `from_millis(1000)`) breaks by
	///   `(scale, value)`, keeping the order consistent with the field-wise `Eq`/`Hash`.
	/// - A scaled and an unscaled timestamp are `None`.
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		let (lhs_scale, rhs_scale) = match (self.scale, other.scale) {
			(None, None) => return Some(self.value.cmp(&other.value)),
			(Some(lhs), Some(rhs)) => (lhs.0.get(), rhs.0.get()),
			_ => return None,
		};
		if lhs_scale == rhs_scale {
			return Some(self.value.cmp(&other.value));
		}
		let lhs = self.value as u128 * rhs_scale as u128;
		let rhs = other.value as u128 * lhs_scale as u128;
		Some(
			lhs.cmp(&rhs)
				.then_with(|| lhs_scale.cmp(&rhs_scale))
				.then_with(|| self.value.cmp(&other.value)),
		)
	}
}

impl From<crate::time::Instant> for Timestamp {
	/// Convert an [`Instant`](crate::time::Instant) into a millisecond-scale timestamp,
	/// measured from the model clock's anchor.
	///
	/// One-way only: there is no inverse, since the anchor is jittered to keep a
	/// [`Timestamp`] from being read back as a clock.
	fn from(instant: crate::time::Instant) -> Self {
		let (anchor, offset) = crate::model::clock::anchor();
		let duration = match instant.checked_duration_since(anchor) {
			Some(forward) => offset + forward,
			None => offset.saturating_sub(anchor.duration_since(instant)),
		};
		Timestamp::from_millis(duration.as_millis() as u64).expect("clock is somehow past the year 2300")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_from_secs() {
		let time = Timestamp::from_secs(5).unwrap();
		assert_eq!(time.scale(), Some(Timescale::SECOND));
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_millis().unwrap(), 5000);
		assert_eq!(time.as_micros().unwrap(), 5_000_000);
		assert_eq!(time.as_nanos().unwrap(), 5_000_000_000);
	}

	#[test]
	fn test_from_millis() {
		let time = Timestamp::from_millis(5000).unwrap();
		assert_eq!(time.scale(), Some(Timescale::MILLI));
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_millis().unwrap(), 5000);
	}

	#[test]
	fn test_from_micros() {
		let time = Timestamp::from_micros(5_000_000).unwrap();
		assert_eq!(time.scale(), Some(Timescale::MICRO));
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_micros().unwrap(), 5_000_000);
	}

	#[test]
	fn test_from_nanos() {
		let time = Timestamp::from_nanos(5_000_000_000).unwrap();
		assert_eq!(time.scale(), Some(Timescale::NANO));
		assert_eq!(time.as_secs().unwrap(), 5);
		assert_eq!(time.as_nanos().unwrap(), 5_000_000_000);
	}

	#[test]
	fn test_timescale_new_rejects_zero_and_overflow() {
		assert!(Timescale::new(0).is_err());
		assert!(Timescale::new(1).is_ok());
		assert_eq!(Timescale::new(1).unwrap(), Timescale::SECOND);
		assert_eq!(Timescale::new(1_000).unwrap(), Timescale::MILLI);

		// Above the QUIC varint range.
		assert!(Timescale::new(1u64 << 62).is_err());
		// Right at the top of the varint range is still valid.
		assert!(Timescale::new((1u64 << 62) - 1).is_ok());
	}

	/// A timestamp is refused where any version could not carry it as a group's first frame,
	/// which lite-05/06 zigzag from zero into a QUIC varint.
	#[test]
	fn test_timestamp_fits_a_lite06_first_frame() {
		let max = Timestamp::new(MAX_TIMESTAMP, Timescale::MICRO).unwrap();
		assert!(crate::coding::varint::zigzag(max.value() as i64) <= MAX_QUIC);

		for value in [MAX_TIMESTAMP + 1, MAX_QUIC] {
			assert!(Timestamp::from_micros(value).is_err(), "{value}");
		}
		assert!(max.checked_add(Timestamp::from_micros(1).unwrap()).is_err());
		assert!(max.convert(Timescale::NANO).is_err());
		assert!(Timestamp::try_from(std::time::Duration::from_nanos(MAX_TIMESTAMP + 1)).is_err());
	}

	#[test]
	fn test_timescale_try_from_nonzero_enforces_varint() {
		use std::num::NonZero;

		assert_eq!(
			Timescale::try_from(NonZero::new(1).unwrap()).unwrap(),
			Timescale::SECOND
		);
		assert_eq!(
			Timescale::try_from(NonZero::new((1u64 << 62) - 1).unwrap())
				.unwrap()
				.as_u64(),
			(1u64 << 62) - 1
		);
		assert!(Timescale::try_from(NonZero::new(1u64 << 62).unwrap()).is_err());
		assert!(Timescale::try_from(NonZero::new(u64::MAX).unwrap()).is_err());
	}

	#[test]
	fn test_convert_to_finer() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_us = time_ms.convert(Timescale::MICRO).unwrap();
		assert_eq!(time_us.scale(), Some(Timescale::MICRO));
		assert_eq!(time_us.as_micros().unwrap(), 5_000_000);
	}

	#[test]
	fn test_convert_to_coarser() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_s = time_ms.convert(Timescale::SECOND).unwrap();
		assert_eq!(time_s.scale(), Some(Timescale::SECOND));
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
	fn test_add_same_scale() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_millis(2000).unwrap();
		let c = a.checked_add(b).unwrap();
		assert_eq!(c.as_millis().unwrap(), 3000);
		assert_eq!(c.scale(), Some(Timescale::MILLI));
	}

	#[test]
	fn test_add_mismatched_scale() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_micros(1000).unwrap();
		assert!(a.checked_add(b).is_err());
	}

	#[test]
	fn test_new_const_matches_fallible() {
		const C: Timestamp = Timestamp::new_const(42, Timescale::MICRO);
		assert_eq!(C, Timestamp::new(42, Timescale::MICRO).unwrap());
	}

	#[test]
	fn test_zero_is_scale_aware() {
		// ZERO is second-scale. is_zero() sees the value regardless of scale, but
		// equality is structural, so it's not interchangeable with 0 at another scale.
		assert!(Timestamp::ZERO.is_zero());
		let zero_ms = Timestamp::from_millis(0).unwrap();
		assert!(zero_ms.is_zero());
		assert_ne!(Timestamp::ZERO, zero_ms);
		assert_ne!(Timestamp::ZERO.partial_cmp(&zero_ms), Some(std::cmp::Ordering::Equal));
	}

	#[test]
	fn test_sub_underflow() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_millis(2000).unwrap();
		assert!(a.checked_sub(b).is_err());
	}

	#[test]
	fn test_ordering_same_scale() {
		let a = Timestamp::from_secs(1).unwrap();
		let b = Timestamp::from_secs(2).unwrap();
		assert!(a < b);
		assert!(b > a);
	}

	#[test]
	fn test_ordering_across_known_scales() {
		// Cross-scale ordering normalizes to a common scale.
		let one_sec = Timestamp::from_secs(1).unwrap();
		let two_ms = Timestamp::from_millis(2).unwrap();
		assert!(one_sec > two_ms);
		assert!(two_ms < one_sec);

		// Temporally-equivalent timestamps with different representations are NOT
		// Equal under cmp: derived Eq compares fields, and Ord must agree.
		let one_sec_b = Timestamp::from_millis(1000).unwrap();
		assert_ne!(one_sec.partial_cmp(&one_sec_b), Some(std::cmp::Ordering::Equal));
		assert_ne!(one_sec, one_sec_b);
		assert_eq!(one_sec.partial_cmp(&one_sec), Some(std::cmp::Ordering::Equal));

		// Mixed-scale sort lands in correct temporal order.
		let mut items = [
			Timestamp::from_secs(2).unwrap(),
			Timestamp::from_millis(500).unwrap(),
			Timestamp::from_micros(1_500_000).unwrap(),
		];
		items.sort_by(|a, b| a.partial_cmp(b).unwrap());
		assert_eq!(items[0], Timestamp::from_millis(500).unwrap());
		assert_eq!(items[1], Timestamp::from_micros(1_500_000).unwrap());
		assert_eq!(items[2], Timestamp::from_secs(2).unwrap());
	}

	#[test]
	fn test_duration_conversion() {
		let duration = std::time::Duration::from_secs(5);
		let time: Timestamp = duration.try_into().unwrap();
		assert_eq!(time.scale(), Some(Timescale::NANO));
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
	}

	/// A scale-less key carries no time: anything needing a scale fails, it orders only
	/// against other keys, and arithmetic stays among keys.
	#[test]
	fn test_unscaled_has_no_time() {
		let key = Timestamp::unscaled(7);
		assert_eq!((key.value(), key.scale()), (7, None));
		assert!(key.convert(Timescale::MILLI).is_err());
		assert!(key.as_millis().is_err());
		assert!(key.as_secs().is_err());
		assert!(std::time::Duration::try_from(key).is_err());
		// A key spans every group key, past where a time stops.
		assert_eq!(Timestamp::unscaled(u64::MAX - 1).value(), u64::MAX - 1);
		assert!(
			Timestamp::unscaled(u64::MAX)
				.checked_add(Timestamp::unscaled(1))
				.is_err()
		);

		let millis = Timestamp::from_millis(7).unwrap();
		assert_ne!(key, millis);
		assert_eq!(key.partial_cmp(&millis), None);
		assert_eq!(millis.partial_cmp(&key), None, "incomparable either way");
		assert!(key.checked_add(millis).is_err());

		let next = Timestamp::unscaled(8);
		assert!(key < next);
		assert_eq!(key.checked_add(Timestamp::unscaled(1)).unwrap(), next);
		assert_eq!(format!("{key:?}"), "#7");
	}

	#[test]
	fn test_custom_scale_convert() {
		// 120 units at 60Hz = 2 seconds, expressed at 1000Hz = 2000 ms.
		let scale_60 = Timescale::new(60).unwrap();
		let t = Timestamp::new(120, scale_60)
			.unwrap()
			.convert(Timescale::MILLI)
			.unwrap();
		assert_eq!(t.scale(), Some(Timescale::MILLI));
		assert_eq!(t.as_millis().unwrap(), 2000);
	}
}
