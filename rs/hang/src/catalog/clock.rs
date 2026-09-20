use serde::{Deserialize, Deserializer, Serialize};

use super::MOQ_EPOCH_UNIX_MILLIS;
use crate::Result;

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
/// Rust. Every media track and the archive index refer to this mapping after timescale conversion.
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
	pub fn wall_clock(&self, pts: moq_net::Timestamp) -> Result<std::time::SystemTime> {
		let scale = self.wall.scale();
		let total = self.wall.value() as u128 + pts.as_scale(scale);
		if total > MAX_SAFE_INTEGER as u128 {
			return Err(crate::Error::InvalidWall(u64::try_from(total).unwrap_or(u64::MAX)));
		}

		let unix_millis = MOQ_EPOCH_UNIX_MILLIS as u128 + total * 1000 / scale.as_u64() as u128;
		let unix_millis =
			u64::try_from(unix_millis).map_err(|_| crate::Error::TimestampOverflow(moq_net::TimeOverflow))?;
		Ok(std::time::UNIX_EPOCH + std::time::Duration::from_millis(unix_millis))
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

	#[test]
	fn wall_clock_converts_across_timescales() {
		let clock = Clock::new(moq_net::Timestamp::from_micros(1_000_000).unwrap()).unwrap();
		let epoch = std::time::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS + 1_000);
		assert_eq!(
			clock.wall_clock(moq_net::Timestamp::from_millis(0).unwrap()).unwrap(),
			epoch
		);

		let second = std::time::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS + 2_000);
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
}
