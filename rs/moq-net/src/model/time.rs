use std::num::NonZero;

use crate::coding::VarInt;

/// Returned when a [`Timestamp`] operation would exceed the QUIC VarInt range
/// (`2^62 - 1`), overflow during scale conversion or arithmetic, or attempt
/// arithmetic between timestamps with mismatched scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// Units per second used by a track for frame timestamps.
///
/// Newtype around [`NonZero<u64>`]. Zero is structurally impossible, so the
/// arithmetic on [`Timestamp`] can divide by `self.scale` without ever risking
/// a divide by zero. Use the named constants ([`Self::SECOND`], [`Self::MILLI`],
/// [`Self::MICRO`], [`Self::NANO`]) instead of writing raw integers at call sites;
/// for runtime values, use [`Self::new`] which returns [`TimeOverflow`] for `0` or
/// for values past the QUIC varint range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timescale(NonZero<u64>);

impl Timescale {
	/// One unit per second (`1`).
	pub const SECOND: Self = Self(NonZero::<u64>::MIN);
	/// 1,000 units per second (`1_000`).
	pub const MILLI: Self = match NonZero::new(1_000) {
		Some(n) => Self(n),
		None => unreachable!(),
	};
	/// 1,000,000 units per second (`1_000_000`). Common default for media tracks.
	pub const MICRO: Self = match NonZero::new(1_000_000) {
		Some(n) => Self(n),
		None => unreachable!(),
	};
	/// 1,000,000,000 units per second (`1_000_000_000`).
	pub const NANO: Self = match NonZero::new(1_000_000_000) {
		Some(n) => Self(n),
		None => unreachable!(),
	};

	/// Construct a timescale from a raw value (units per second).
	///
	/// Returns [`TimeOverflow`] if `units_per_second` is `0` (would divide by zero)
	/// or exceeds `2^62 - 1` (the QUIC varint range, matching [`Timestamp`] values).
	pub const fn new(units_per_second: u64) -> Result<Self, TimeOverflow> {
		// Reject values that wouldn't fit in a QUIC varint, keeping the constraint
		// symmetric with Timestamp's raw value.
		if VarInt::from_u64(units_per_second).is_none() {
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

impl From<NonZero<u64>> for Timescale {
	fn from(units_per_second: NonZero<u64>) -> Self {
		Self(units_per_second)
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

impl Default for Timescale {
	/// Milliseconds ([`Self::MILLI`]). Every track has a timescale; this is the one
	/// used when a producer doesn't pick one and the fallback for protocols whose wire
	/// can't carry a timescale (pre-Lite05 moq-lite, IETF moq-transport).
	fn default() -> Self {
		Self::MILLI
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
/// All timestamps within a track are relative, so zero for one track is not zero for another.
/// The underlying value is constrained to fit within a QUIC VarInt (`2^62 - 1`) so it can be
/// encoded and decoded easily; the scale is carried alongside so frames from different
/// sources can be compared and converted without lossy detours through a single fixed scale.
///
/// The scale is a [`Timescale`] (always non-zero), so unit conversions (`as_secs`, `as_millis`,
/// etc.) are infallible. Use [`Option<Timestamp>`] at call sites that need a "missing" sentinel
/// instead of relying on a magic value.
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
/// - [`Ord`] is **temporal**: it cross-multiplies scales, so `from_millis(1000)` orders after
///   `from_millis(999)` and `from_secs(1)` slots in between. When a cross-scale comparison is
///   otherwise a tie, it breaks by `(scale, value)` to stay consistent with `Eq`.
///
/// So `from_secs(1).cmp(&from_millis(1000))` is *not* `Equal`, and neither is `==` true. If you
/// want "same instant regardless of encoding", compare after a [`Self::convert`] to a common scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
	value: VarInt,
	scale: Timescale,
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
	/// Returns [`TimeOverflow`] if `value` exceeds `2^62 - 1`.
	pub const fn new(value: u64, scale: Timescale) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(value) {
			Some(value) => Ok(Self { value, scale }),
			None => Err(TimeOverflow),
		}
	}

	/// Const-context twin of [`Self::new`] that panics on overflow.
	///
	/// For building `const` timestamps where `?`/`unwrap` on the [`Result`] isn't
	/// available. The panic fires only on a compile-time-known out-of-range literal, so
	/// it's a build-time assertion, not a runtime failure path. Use [`Self::new`]
	/// everywhere else.
	pub const fn new_const(value: u64, scale: Timescale) -> Self {
		match Self::new(value, scale) {
			Ok(time) => time,
			Err(_) => panic!("timestamp value exceeds 2^62 - 1"),
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

	/// The raw value in the timestamp's own scale.
	pub const fn value(self) -> u64 {
		self.value.into_inner()
	}

	/// The scale (units per second) attached to this timestamp.
	pub const fn scale(self) -> Timescale {
		self.scale
	}

	/// Whether the raw value is zero. Does not consider scale.
	pub const fn is_zero(self) -> bool {
		self.value.into_inner() == 0
	}

	/// Re-express this timestamp at a new scale. Returns [`TimeOverflow`] if the new
	/// value would exceed `2^62 - 1`.
	pub const fn convert(self, new_scale: Timescale) -> Result<Self, TimeOverflow> {
		if self.scale.0.get() == new_scale.0.get() {
			return Ok(self);
		}
		match (self.value.into_inner() as u128).checked_mul(new_scale.0.get() as u128) {
			Some(scaled) => match VarInt::from_u128(scaled / self.scale.0.get() as u128) {
				Some(value) => Ok(Self {
					value,
					scale: new_scale,
				}),
				None => Err(TimeOverflow),
			},
			None => Err(TimeOverflow),
		}
	}

	/// The value re-expressed at `target` as a `u128`.
	pub const fn as_scale(self, target: Timescale) -> u128 {
		self.value.into_inner() as u128 * target.0.get() as u128 / self.scale.0.get() as u128
	}

	/// The value re-expressed in seconds.
	pub const fn as_secs(self) -> u64 {
		self.value.into_inner() / self.scale.0.get()
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

	/// Add two timestamps. Returns [`TimeOverflow`] if the sum exceeds `2^62 - 1` or
	/// if the scales differ.
	pub const fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale.0.get() != rhs.scale.0.get() {
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
		if self.scale.0.get() != rhs.scale.0.get() {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_sub(rhs.value.into_inner()) {
			Some(result) => Self::new(result, self.scale),
			None => Err(TimeOverflow),
		}
	}

	/// Current point on the local monotonic clock, expressed in the default timescale
	/// ([`Timescale::MILLI`]).
	///
	/// This is the one-way bridge from a local clock to a track timestamp: there is
	/// deliberately no inverse (a [`Timestamp`] is relative and jittered, never a clock).
	/// Used to stamp frames that arrive without one, e.g. on protocols whose wire can't
	/// carry a timestamp. Uses [`web_async::time::Instant::now`] so it works on wasm and honors
	/// `tokio::time::pause` in tests.
	pub fn now() -> Self {
		clock::now()
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
		let nanos = time.as_nanos();
		std::time::Duration::new(time.as_secs(), (nanos % 1_000_000_000) as u32)
	}
}

impl std::fmt::Debug for Timestamp {
	#[allow(clippy::manual_is_multiple_of)] // is_multiple_of is unstable in Rust 1.85
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let nanos = self.as_nanos();

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
	/// Temporal comparison, normalizing across scales (see the type-level docs for how
	/// this relates to structural `Eq`).
	///
	/// - Equal scales compare raw values directly.
	/// - Otherwise cross-multiplies in 128-bit so e.g. `1s > 2ms` orders correctly.
	/// - A would-be cross-scale tie (e.g. `from_secs(1)` vs `from_millis(1000)`) breaks by
	///   `(scale, value)`, keeping `Ord` consistent with the field-wise `Eq`/`Hash`.
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		if self.scale.0.get() == other.scale.0.get() {
			return self.value.cmp(&other.value);
		}
		let lhs = self.value.into_inner() as u128 * other.scale.0.get() as u128;
		let rhs = other.value.into_inner() as u128 * self.scale.0.get() as u128;
		lhs.cmp(&rhs)
			.then_with(|| self.scale.0.get().cmp(&other.scale.0.get()))
			.then_with(|| self.value.cmp(&other.value))
	}
}

#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
mod clock {
	use std::sync::LazyLock;
	use std::time::{SystemTime, UNIX_EPOCH};

	use rand::RngExt;

	use super::Timestamp;

	/// Epoch the wall-clock timestamps are measured from: 2020-01-01T00:00:00Z.
	///
	/// A [`Timestamp`] isn't a real clock, it just needs to be non-negative and roughly
	/// monotonic with wall time. Anchoring 50 years after the Unix epoch keeps the value
	/// ~1.5e12 ms smaller, trimming a byte or two off the first frame's varint.
	const ANCHOR_EPOCH_SECS: u64 = 1_577_836_800;

	// There's no zero Instant, so we need to use a reference point.
	static TIME_ANCHOR: LazyLock<(std::time::Instant, SystemTime)> = LazyLock::new(|| {
		// To deter nerds trying to use timestamp as wall clock time, we subtract a random amount of time from the anchor.
		// This will make our timestamps appear to be late; just enough to be annoying and obscure our clock drift.
		// This will also catch bad implementations that assume unrelated broadcasts are synchronized.
		let jitter = std::time::Duration::from_millis(rand::rng().random_range(0..69_420));
		(std::time::Instant::now(), SystemTime::now() - jitter)
	});

	pub(super) fn now() -> Timestamp {
		let instant: std::time::Instant = web_async::time::Instant::now().into();
		from_std_instant(instant)
	}

	fn from_std_instant(instant: std::time::Instant) -> Timestamp {
		let (anchor_instant, anchor_system) = *TIME_ANCHOR;

		let system = match instant.checked_duration_since(anchor_instant) {
			Some(forward) => anchor_system + forward,
			None => anchor_system - anchor_instant.duration_since(instant),
		};

		let epoch = UNIX_EPOCH + std::time::Duration::from_secs(ANCHOR_EPOCH_SECS);
		// Saturate to zero rather than panic if the wall clock is before 2020 (an unsynced
		// clock on a peer-driven path), since the only requirement is a non-negative start.
		let duration = system.duration_since(epoch).unwrap_or(std::time::Duration::ZERO);

		Timestamp::from_millis(duration.as_millis() as u64).expect("clock is somehow past the year 2300")
	}

	impl From<std::time::Instant> for Timestamp {
		/// Convert an [`std::time::Instant`] into a millisecond-scale timestamp (the default
		/// timescale), anchored at 2020-01-01 plus a per-process jitter (see `TIME_ANCHOR`).
		///
		/// One-way only: there is no inverse, since the anchor is jittered to keep a
		/// [`Timestamp`] from being read back as a clock.
		fn from(instant: std::time::Instant) -> Self {
			from_std_instant(instant)
		}
	}

	impl From<tokio::time::Instant> for Timestamp {
		fn from(instant: tokio::time::Instant) -> Self {
			from_std_instant(instant.into_std())
		}
	}
}

#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
mod clock {
	use std::sync::LazyLock;

	use rand::RngExt;

	use super::Timestamp;

	static TIME_ANCHOR: LazyLock<(web_async::time::Instant, std::time::Duration)> = LazyLock::new(|| {
		let jitter = std::time::Duration::from_millis(rand::rng().random_range(1..69_420));
		(web_async::time::Instant::now(), jitter)
	});

	pub(super) fn now() -> Timestamp {
		let (anchor_instant, anchor_duration) = *TIME_ANCHOR;
		let instant = web_async::time::Instant::now();
		let duration = match instant.checked_duration_since(anchor_instant) {
			Some(forward) => anchor_duration + forward,
			None => anchor_duration
				.checked_sub(anchor_instant.duration_since(instant))
				.unwrap_or(std::time::Duration::ZERO),
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
		assert_ne!(Timestamp::ZERO.cmp(&zero_ms), std::cmp::Ordering::Equal);
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
	fn test_max_cross_scale() {
		// `Ord::max` compares across scales (no panic).
		let a = Timestamp::from_millis(1).unwrap();
		let b = Timestamp::from_secs(1).unwrap();
		assert_eq!(a.max(b), b);
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
	fn test_ordering_across_known_scales() {
		// Cross-scale ordering normalizes to a common scale.
		let one_sec = Timestamp::from_secs(1).unwrap();
		let two_ms = Timestamp::from_millis(2).unwrap();
		assert!(one_sec > two_ms);
		assert!(two_ms < one_sec);

		// Temporally-equivalent timestamps with different representations are NOT
		// Equal under cmp: derived Eq compares fields, and Ord must agree.
		let one_sec_b = Timestamp::from_millis(1000).unwrap();
		assert_ne!(one_sec.cmp(&one_sec_b), std::cmp::Ordering::Equal);
		assert_ne!(one_sec, one_sec_b);
		assert_eq!(one_sec.cmp(&one_sec), std::cmp::Ordering::Equal);

		// Mixed-scale sort lands in correct temporal order.
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
	fn test_custom_scale_convert() {
		// 120 units at 60Hz = 2 seconds, expressed at 1000Hz = 2000 ms.
		let scale_60 = Timescale::new(60).unwrap();
		let t = Timestamp::new(120, scale_60)
			.unwrap()
			.convert(Timescale::MILLI)
			.unwrap();
		assert_eq!(t.scale(), Timescale::MILLI);
		assert_eq!(t.as_millis(), 2000);
	}
}
