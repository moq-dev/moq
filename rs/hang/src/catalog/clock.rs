use serde::{Deserialize, Deserializer, Serialize};

use crate::Result;

/// The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds.
pub const MOQ_EPOCH_UNIX_MILLIS: u64 = 1_577_836_800_000;

/// The largest integer JSON preserves exactly (2^53 - 1).
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn deserialize_wall<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
	D: Deserializer<'de>,
{
	let value = u64::deserialize(deserializer)?;
	if value > MAX_SAFE_INTEGER {
		return Err(serde::de::Error::custom(format!("invalid wall clock: {value}")));
	}
	Ok(value)
}

const fn default_timescale() -> u32 {
	1_000_000
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
	#[serde(deserialize_with = "deserialize_wall")]
	wall: u64,
	#[serde(
		default = "default_timescale",
		deserialize_with = "super::deserialize_timescale_or_default"
	)]
	timescale: u32,
}

/// The broadcast's one continuous clock, advertised at the catalog root.
///
/// [`wall`](Self::wall) is the wall-clock time of PTS zero since the moq epoch
/// ([`MOQ_EPOCH_UNIX_MILLIS`], 2020-01-01). Its embedded timescale is serialized beside it as
/// `{ wall, timescale }`, preserving the catalog wire shape while keeping the pair inseparable in
/// Rust. Every media track and every HLS record refer to this mapping after timescale conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Clock {
	/// The wall-clock time of PTS zero since the moq epoch.
	pub wall: moq_net::Timestamp,
}

impl Clock {
	/// Construct a catalog clock from its typed wall timestamp.
	///
	/// Refuses a value outside the JSON-safe integer range or a timescale that cannot be represented
	/// by the catalog's `u32` wire field.
	pub fn new(wall: moq_net::Timestamp) -> Result<Self> {
		if wall.value() > MAX_SAFE_INTEGER {
			return Err(crate::Error::InvalidWall(wall.value()));
		}
		if wall.scale().as_u64() > u32::MAX as u64 {
			return Err(crate::Error::InvalidTimescale(wall.scale().as_u64()));
		}
		Ok(Self { wall })
	}

	/// The wall-clock time of `pts` under this fixed mapping.
	///
	/// Exact down to the nanosecond; an instant finer than that (a 90 kHz tick) is floored.
	/// Refuses a result past the JSON-safe range or the wall clock's 1970-2554 range.
	pub fn wall_clock(&self, pts: moq_net::Timestamp) -> Result<kio::clock::Wall> {
		let overflow = || crate::Error::TimestampOverflow(moq_net::TimeOverflow);
		let wall_scale = self.wall.scale().as_u64() as u128;
		let pts_scale = pts.scale().as_u64() as u128;

		// Sum as one exact fraction of a second, `num / den`, so neither side is rounded into the
		// other's units first.
		let num = (self.wall.value() as u128)
			.checked_mul(pts_scale)
			.and_then(|wall| wall.checked_add((pts.value() as u128).checked_mul(wall_scale)?))
			.ok_or_else(overflow)?;
		let den = wall_scale.checked_mul(pts_scale).ok_or_else(overflow)?;

		// `num / pts_scale` is the result in the wall's units; any fraction past the limit is past it.
		if num > MAX_SAFE_INTEGER as u128 * pts_scale {
			let total = num.div_ceil(pts_scale);
			return Err(crate::Error::InvalidWall(u64::try_from(total).unwrap_or(u64::MAX)));
		}

		let secs = u64::try_from(num / den).map_err(|_| overflow())?;
		let nanos = (num % den).checked_mul(1_000_000_000).ok_or_else(overflow)? / den;
		let since_moq_epoch = std::time::Duration::new(secs, nanos as u32);
		// A catalog can name a wall beyond the clock's nanosecond range.
		kio::clock::Wall::UNIX_EPOCH
			.checked_add(std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS))
			.and_then(|moq_epoch| moq_epoch.checked_add(since_moq_epoch))
			.ok_or_else(overflow)
	}
}

impl Serialize for Clock {
	fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		Wire {
			wall: self.wall.value(),
			timescale: self.wall.scale().as_u64() as u32,
		}
		.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Clock {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let wire = Wire::deserialize(deserializer)?;
		let wall =
			moq_net::Timestamp::from_scale(wire.wall, wire.timescale as u64).map_err(serde::de::Error::custom)?;
		Self::new(wall).map_err(serde::de::Error::custom)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn defaults_timescale_to_micros() {
		let decoded: Clock = serde_json::from_str(r#"{"wall":1000}"#).unwrap();
		assert_eq!(decoded.wall, moq_net::Timestamp::from_micros(1000).unwrap());
		assert_eq!(
			serde_json::to_string(&decoded).unwrap(),
			r#"{"wall":1000,"timescale":1000000}"#
		);
	}

	#[test]
	fn roundtrip() {
		let clock = Clock::new(moq_net::Timestamp::from_millis(175_184_640_000).unwrap()).unwrap();
		let json = serde_json::to_string(&clock).unwrap();
		assert_eq!(json, r#"{"wall":175184640000,"timescale":1000}"#);
		assert_eq!(serde_json::from_str::<Clock>(&json).unwrap(), clock);
	}

	#[test]
	fn zero_timescale_is_refused() {
		serde_json::from_str::<Clock>(r#"{"wall":0,"timescale":0}"#).expect_err("a zero timescale must not decode");
	}

	#[test]
	fn explicit_null_timescale_is_refused() {
		serde_json::from_str::<Clock>(r#"{"wall":0,"timescale":null}"#)
			.expect_err("an explicit null timescale must not decode as the default");
	}

	#[test]
	fn wall_beyond_json_safe_integers_is_refused() {
		serde_json::from_str::<Clock>(r#"{"wall":9007199254740992}"#).expect_err("a wall past 2^53-1 must not decode");
		let wall = moq_net::Timestamp::new(MAX_SAFE_INTEGER + 1, moq_net::Timescale::MICRO).unwrap();
		assert!(Clock::new(wall).is_err());
	}

	/// A fraction of a wall unit past the JSON-safe range is still past it.
	#[test]
	fn wall_clock_refuses_a_fraction_past_the_safe_range() {
		let clock = Clock::new(moq_net::Timestamp::from_micros(MAX_SAFE_INTEGER).unwrap()).unwrap();
		clock.wall_clock(moq_net::Timestamp::ZERO).unwrap();
		assert!(matches!(
			clock.wall_clock(moq_net::Timestamp::from_nanos(1).unwrap()),
			Err(crate::Error::InvalidWall(_))
		));
	}

	/// A JSON-safe catalog wall can still exceed Wall's nanosecond range.
	#[test]
	fn wall_clock_past_the_wall_range_is_an_error() {
		let wall = moq_net::Timestamp::new(MAX_SAFE_INTEGER, moq_net::Timescale::SECOND).unwrap();
		let clock = Clock::new(wall).unwrap();
		assert!(matches!(
			clock.wall_clock(moq_net::Timestamp::ZERO),
			Err(crate::Error::TimestampOverflow(_))
		));
	}

	#[test]
	fn wall_clock_converts_across_timescales() {
		let clock = Clock::new(moq_net::Timestamp::from_micros(1_000_000).unwrap()).unwrap();
		let epoch = kio::clock::Wall::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS + 1_000);
		assert_eq!(
			clock.wall_clock(moq_net::Timestamp::from_millis(0).unwrap()).unwrap(),
			epoch
		);

		let second = kio::clock::Wall::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS + 2_000);
		assert_eq!(
			clock
				.wall_clock(moq_net::Timestamp::from_millis(1000).unwrap())
				.unwrap(),
			second
		);
		assert_eq!(
			clock
				.wall_clock(moq_net::Timestamp::from_scale(48_000, 48_000).unwrap())
				.unwrap(),
			second
		);
		assert_eq!(
			clock
				.wall_clock(moq_net::Timestamp::from_scale(90_000, 90_000).unwrap())
				.unwrap(),
			second
		);
	}

	/// Regression: the result was floored to the millisecond, dropping the sub-millisecond part.
	#[test]
	fn wall_clock_keeps_sub_millisecond_precision() {
		let moq_epoch = kio::clock::Wall::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS);
		let micros = |v| moq_net::Timestamp::from_micros(v).unwrap();
		let nanos = std::time::Duration::from_nanos;

		let clock = Clock::new(micros(1_000_999)).unwrap();
		assert_eq!(
			clock.wall_clock(moq_net::Timestamp::ZERO).unwrap(),
			moq_epoch + nanos(1_000_999_000)
		);

		// One 90 kHz tick on a microsecond wall is 11,111.1 ns, floored at the nanosecond.
		let tick = moq_net::Timestamp::from_scale(1, 90_000).unwrap();
		assert_eq!(
			clock.wall_clock(tick).unwrap(),
			moq_epoch + nanos(1_000_999_000 + 11_111)
		);

		// A 1 µs pts on a millisecond wall is not lost converting into the wall's units.
		let clock = Clock::new(moq_net::Timestamp::from_millis(1_000).unwrap()).unwrap();
		assert_eq!(clock.wall_clock(micros(1)).unwrap(), moq_epoch + nanos(1_000_001_000));
	}
}
