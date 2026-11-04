use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::millis::MillisCeil;

/// What applies to every audio and video track of a broadcast: its clock and how a viewer joins.
///
/// Serialized under the catalog root's `media` key, and omitted when it holds nothing.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Media {
	/// The broadcast's one continuous clock, if the publisher exposes one.
	///
	/// `wall` is the wall-clock time of PTS zero in `timescale` units since the moq epoch
	/// (2020-01-01); every media track refers to this mapping after timescale conversion. See
	/// [`Clock`](crate::catalog::Clock).
	#[serde(default)]
	pub clock: Option<crate::catalog::Clock>,

	/// How often a viewer can join every media track, if the publisher promises it.
	///
	/// An undecodable value is absent, so it can never reject the media.
	#[serde(default)]
	#[serde_as(as = "serde_with::DefaultOnError<_>")]
	pub join: Option<Join>,
}

impl Media {
	/// Whether the section holds nothing, so the catalog leaves it off the wire.
	pub fn is_empty(&self) -> bool {
		self.clock.is_none() && self.join.is_none()
	}

	/// The declared [`Join::interval`], if any.
	pub fn interval(&self) -> Option<Duration> {
		self.join.map(|join| join.interval)
	}
}

/// How a viewer joins the broadcast's media tracks.
///
/// Nested so that a later join mechanism that is not a fixed grid can sit beside the interval.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase", try_from = "JoinWire")]
#[non_exhaustive]
pub struct Join {
	/// The promise that every audio and video track starts a group at least this often outside a
	/// pause: the longest group (GOP) any of them publishes.
	///
	/// It is also the grid an HLS or DASH export numbers its segments on, so every rendition shares
	/// segment numbers, and it is fixed for the broadcast instance since that numbering depends on
	/// it. A broadcast without one is not HLS-renderable. Serialized as whole milliseconds, rounded
	/// up; zero is refused.
	#[serde_as(as = "MillisCeil")]
	pub interval: Duration,
}

/// [`Join`] as decoded, before its interval is checked.
#[serde_with::serde_as]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JoinWire {
	#[serde_as(as = "MillisCeil")]
	interval: Duration,
}

impl TryFrom<JoinWire> for Join {
	type Error = &'static str;

	fn try_from(wire: JoinWire) -> Result<Self, Self::Error> {
		Self::new(wire.interval).ok_or("a join interval must be positive")
	}
}

impl Join {
	/// Promise a group at least every `interval`, or `None` for zero, which readers decode as no
	/// promise at all.
	pub fn new(interval: Duration) -> Option<Self> {
		(!interval.is_zero()).then_some(Self { interval })
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrips_in_milliseconds() {
		let media = Media {
			join: Join::new(Duration::from_micros(2_000_500)),
			..Default::default()
		};
		let json = serde_json::to_string(&media).unwrap();
		assert_eq!(json, r#"{"join":{"interval":2001}}"#);
		assert_eq!(
			serde_json::from_str::<Media>(&json).unwrap().interval(),
			Some(Duration::from_millis(2001))
		);
	}

	#[test]
	fn a_zero_interval_is_no_promise() {
		assert_eq!(Join::new(Duration::ZERO), None);
		// Anything above zero rounds up to a whole millisecond on the wire, which reads back.
		let join = Join::new(Duration::from_nanos(1)).unwrap();
		let media: Media = serde_json::from_str(&serde_json::to_string(&Media {
			join: Some(join),
			..Default::default()
		})
		.unwrap())
		.unwrap();
		assert_eq!(media.interval(), Some(Duration::from_millis(1)));
	}

	#[test]
	fn an_undecodable_join_is_absent() {
		for join in [
			r#"{"interval":0}"#,
			r#"{"interval":-1}"#,
			r#"{"interval":1.5}"#,
			r#"{}"#,
			r#""2000""#,
			"null",
		] {
			let media: Media = serde_json::from_str(&format!(r#"{{"join":{join}}}"#)).unwrap();
			assert_eq!(media.join, None, "{join}");
		}
	}
}
