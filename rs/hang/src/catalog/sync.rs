use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::millis::MillisCeil;

/// The broadcast's timeline: the wall-clock time of PTS zero and how often a viewer can join.
///
/// Serialized under the catalog root's `sync` key, and omitted when it holds nothing.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Sync {
	/// The broadcast's one continuous clock, if the publisher exposes one.
	///
	/// `wall` is the wall-clock time of PTS zero in `timescale` units since the moq epoch
	/// (2020-01-01); every track refers to this mapping after timescale conversion. See
	/// [`Clock`](crate::catalog::Clock).
	#[serde(default, deserialize_with = "crate::catalog::deserialize_some")]
	pub clock: Option<crate::catalog::Clock>,

	/// The promise that every audio and video track starts a group at least this often outside a
	/// pause: the longest group (GOP) any of them publishes.
	///
	/// It is also the grid an HLS or DASH export numbers its segments on, so every rendition shares
	/// segment numbers, and it is fixed for the broadcast instance since that numbering depends on
	/// it. A broadcast without one is not HLS-renderable. Serialized as whole milliseconds, rounded
	/// up. Zero is no promise: encoding refuses it, and so does a reader.
	#[serde(
		default,
		serialize_with = "serialize_interval",
		deserialize_with = "deserialize_interval"
	)]
	pub interval: Option<Duration>,
}

impl Sync {
	/// Whether the section holds nothing, so the catalog leaves it off the wire.
	pub fn is_empty(&self) -> bool {
		self.clock.is_none() && self.interval.is_none()
	}
}

/// Encode the interval in whole milliseconds, rounded up, refusing zero.
fn serialize_interval<S: serde::Serializer>(interval: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error> {
	<MillisCeil as serde_with::SerializeAs<Option<Duration>>>::serialize_as(interval, serializer)
}

/// Decode a present `interval`, which must be a positive whole number of milliseconds.
fn deserialize_interval<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
	let millis = <crate::snapshot::Number as serde_with::DeserializeAs<'de, u64>>::deserialize_as(deserializer)?;
	if millis == 0 {
		return Err(serde::de::Error::custom("a sync interval must be positive"));
	}
	Ok(Some(Duration::from_millis(millis)))
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrips_in_milliseconds() {
		let sync = Sync {
			interval: Some(Duration::from_micros(2_000_500)),
			..Default::default()
		};
		let json = serde_json::to_string(&sync).unwrap();
		assert_eq!(json, r#"{"interval":2001}"#);
		assert_eq!(
			serde_json::from_str::<Sync>(&json).unwrap().interval,
			Some(Duration::from_millis(2001))
		);
	}

	#[test]
	fn a_zero_interval_is_no_promise() {
		let zero = Sync {
			interval: Some(Duration::ZERO),
			..Default::default()
		};
		serde_json::to_string(&zero).expect_err("a zero interval must not encode");

		// Anything above zero rounds up to a whole millisecond on the wire, which reads back.
		let tiny = Sync {
			interval: Some(Duration::from_nanos(1)),
			..Default::default()
		};
		let sync: Sync = serde_json::from_str(&serde_json::to_string(&tiny).unwrap()).unwrap();
		assert_eq!(sync.interval, Some(Duration::from_millis(1)));
	}

	#[test]
	fn an_undecodable_interval_refuses() {
		for interval in ["0", "-1", "1.5", r#""2000""#, "{}", "[2000]", "null"] {
			serde_json::from_str::<Sync>(&format!(r#"{{"interval":{interval}}}"#)).expect_err(interval);
		}
	}
}
