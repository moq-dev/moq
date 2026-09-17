use serde::{Deserialize, Deserializer, Serialize};

use super::MOQ_EPOCH_UNIX_MILLIS;
use crate::Result;

/// The largest integer JSON preserves exactly (2^53 - 1).
///
/// Catalog wall values must fit here so browser consumers read the same number the publisher
/// wrote. Anything larger is refused rather than truncated.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Deserialize a wall value, refusing anything outside the JSON-safe integer range.
pub(crate) fn deserialize_wall<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
	D: Deserializer<'de>,
{
	let value = u64::deserialize(deserializer)?;
	if value > MAX_SAFE_INTEGER {
		return Err(serde::de::Error::custom(format!("invalid wall clock: {value}")));
	}
	Ok(value)
}

/// The broadcast's one continuous clock, advertised at the catalog root.
///
/// `wall` is the wall-clock time of PTS zero, in [`timescale`](Self::timescale) units since the
/// moq epoch ([`MOQ_EPOCH_UNIX_MILLIS`], 2020-01-01). A consumer derives the wall-clock time of
/// any media timestamp as `wall + pts` after converting that timestamp into this timescale, and
/// Unix time by adding the moq epoch back (an absolute clock for HLS
/// `EXT-X-PROGRAM-DATE-TIME` / DASH `availabilityStartTime`).
///
/// There is one mapping per broadcast: every media track and the archive index refer to it after
/// timescale conversion, so there are no competing wall epochs. The publisher fixes it once and
/// never overwrites it: a discontinuity marker is a delivery event, not a new epoch, and a
/// system-clock adjustment never retimes it. It is independent of
/// [`Archive`](super::Archive): a live-only publisher exposes its clock without creating a
/// segment index.
///
/// Measured from 2020 rather than 1970 so the value stays small and safely within a 53-bit
/// integer even at fine timescales.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Clock {
	/// The wall-clock time of PTS zero, in [`timescale`](Self::timescale) units since the moq
	/// epoch. Must fit in a JSON-safe integer.
	#[serde(deserialize_with = "deserialize_wall")]
	pub wall: u64,

	/// Units per second for [`wall`](Self::wall). Defaults to 1,000,000 (microseconds), the
	/// broadcast clock's own timescale. An omitted field takes that default; an explicit
	/// null is refused.
	#[serde(
		default = "Clock::default_timescale",
		deserialize_with = "deserialize_timescale_or_default"
	)]
	pub timescale: u32,
}

pub(crate) fn deserialize_timescale_or_default<'de, D>(deserializer: D) -> std::result::Result<u32, D::Error>
where
	D: Deserializer<'de>,
{
	// A missing field uses the serde `default`; an explicit null is not a number and is refused.
	let value = u32::deserialize(deserializer)?;
	if value == 0 {
		return Err(serde::de::Error::custom("invalid timescale: 0"));
	}
	Ok(value)
}

impl Clock {
	/// The default timescale (1,000,000, i.e. microseconds) for a clock section that omits the
	/// field. Matches the broadcast clock's own timescale.
	pub fn default_timescale() -> u32 {
		1_000_000
	}

	/// A clock section with this `wall` at the default microsecond timescale.
	///
	/// Errors on a wall outside the JSON-safe integer range, since a browser would read a
	/// different number than the publisher wrote.
	pub fn new(wall: u64) -> Result<Self> {
		Self::with_timescale(wall, Self::default_timescale())
	}

	/// A clock section with this `wall` and `timescale`.
	///
	/// Errors on a zero timescale or a wall outside the JSON-safe integer range, rather than
	/// publishing a mapping no consumer can convert or read back exactly.
	pub fn with_timescale(wall: u64, timescale: u32) -> Result<Self> {
		if timescale == 0 {
			return Err(crate::Error::InvalidTimescale(0));
		}
		if wall > MAX_SAFE_INTEGER {
			return Err(crate::Error::InvalidWall(wall));
		}
		Ok(Self { wall, timescale })
	}

	/// The wall-clock time of `pts`, given in `pts_timescale` units per second.
	///
	/// Converts `pts` into this clock's timescale explicitly, then applies the fixed mapping
	/// `wall + pts`. Refuses a zero `pts_timescale`, an out-of-range result, or a timestamp the
	/// timescales cannot represent, rather than truncating.
	pub fn wall_clock(&self, pts: u64, pts_timescale: u32) -> Result<std::time::SystemTime> {
		if self.timescale == 0 {
			return Err(crate::Error::InvalidTimescale(0));
		}
		if pts_timescale == 0 {
			return Err(crate::Error::InvalidTimescale(0));
		}
		let scale = moq_net::Timescale::new(self.timescale as u64)?;
		let pts = moq_net::Timestamp::new(pts, moq_net::Timescale::new(pts_timescale as u64)?)?;
		let units = pts.as_scale(scale);

		let total = self.wall as u128 + units;
		if total > MAX_SAFE_INTEGER as u128 {
			return Err(crate::Error::InvalidWall(total as u64));
		}

		let unix_millis = MOQ_EPOCH_UNIX_MILLIS as u128 + total * 1000 / scale.as_u64() as u128;
		let unix_millis =
			u64::try_from(unix_millis).map_err(|_| crate::Error::TimestampOverflow(moq_net::TimeOverflow))?;
		Ok(std::time::UNIX_EPOCH + std::time::Duration::from_millis(unix_millis))
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn defaults_timescale_to_micros() {
		let decoded: Clock = serde_json::from_str(r#"{"wall":1000}"#).unwrap();
		assert_eq!(decoded.timescale, 1_000_000);
		assert_eq!(
			serde_json::to_string(&decoded).unwrap(),
			r#"{"wall":1000,"timescale":1000000}"#
		);
	}

	#[test]
	fn roundtrip() {
		let clock = Clock {
			wall: 175_184_640_000_000,
			timescale: 1000,
		};
		let json = serde_json::to_string(&clock).unwrap();
		assert_eq!(json, r#"{"wall":175184640000000,"timescale":1000}"#);
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
		assert!(Clock::new(MAX_SAFE_INTEGER + 1).is_err());
	}

	#[test]
	fn wall_clock_converts_across_timescales() {
		// PTS zero is the wall epoch itself.
		let clock = Clock::new(1_000_000).unwrap();
		let epoch = std::time::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS + 1_000);
		assert_eq!(clock.wall_clock(0, 1000).unwrap(), epoch);

		// One media second later, whatever timescale names it.
		let second = std::time::UNIX_EPOCH + std::time::Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS + 2_000);
		assert_eq!(clock.wall_clock(1000, 1000).unwrap(), second);
		assert_eq!(clock.wall_clock(48_000, 48_000).unwrap(), second);
		assert_eq!(clock.wall_clock(90_000, 90_000).unwrap(), second);
	}

	#[test]
	fn wall_clock_refuses_a_zero_pts_timescale() {
		let clock = Clock::new(0).unwrap();
		assert!(clock.wall_clock(0, 0).is_err());
	}
}
