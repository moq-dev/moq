use serde::{Deserialize, Serialize};

/// The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds.
///
/// Timeline [`wall`](crate::catalog::Clock::wall) values are measured from here rather than the Unix epoch so
/// the numbers stay small (and safely within a 53-bit integer even at fine timescales); a
/// consumer recovers Unix time by adding this back.
pub const MOQ_EPOCH_UNIX_MILLIS: u64 = 1_577_836_800_000;

/// Describes the broadcast's timeline track, its segment index.
///
/// The timeline track carries one record per aligned segment: a span of content time shared
/// by every media track, mapped to the group ranges that carry it on each track (see the
/// [`timeline`](crate::timeline) module for the record format). A consumer can seek, or build
/// an HLS/DASH playlist, without downloading the media itself.
///
/// The section lives inside the catalog's root [`Archive`](crate::catalog::Archive) (flattened
/// on the wire): there is one timeline per broadcast, because its whole point is that
/// segments are aligned across the broadcast's tracks. A publisher that doesn't segment
/// simply omits the archive entry. Wall-clock mapping is the catalog root
/// [`Clock`](crate::catalog::Clock)'s job, not this section's: every track and this index
/// refer to that one mapping after timescale conversion.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Timeline {
	/// The name of the MoQ track carrying the broadcast's segment records
	/// ([`timeline::DEFAULT_NAME`](crate::timeline::DEFAULT_NAME) by convention).
	pub track: String,

	/// Units per second for the timeline's `pts`. Defaults to 1000
	/// (milliseconds). Zero is refused: no timestamp can be expressed in it.
	#[serde(
		default = "Timeline::default_timescale",
		deserialize_with = "super::deserialize_timescale_or_default"
	)]
	pub timescale: u32,

	/// The declared upper bound on a segment's duration, in [`timescale`](Self::timescale)
	/// units, when the publisher can promise one.
	///
	/// A publisher that controls its encoder knows its keyframe cadence up front, so a consumer
	/// can size buffers or write an HLS `EXT-X-TARGETDURATION` from the catalog alone, before
	/// observing a single segment. No record ever exceeds it: the publisher fails the timeline
	/// rather than contradict this.
	///
	/// Absent when the media decides instead, which is the common case for real-time (where a
	/// GOP can be minutes long) and for a publisher importing a source it doesn't control. A
	/// consumer needing a bound then derives one from the records it has seen.
	pub duration_max: Option<u64>,
}

impl Timeline {
	/// The default timescale (1000, i.e. milliseconds) for a timeline whose catalog section
	/// omits the field.
	pub fn default_timescale() -> u32 {
		1000
	}

	/// A timeline section naming `track`, at the default millisecond timescale, with no
	/// declared duration bound. Set [`timescale`](Self::timescale) /
	/// [`duration_max`](Self::duration_max) afterward.
	pub fn new(track: impl Into<String>) -> Self {
		Self {
			track: track.into(),
			timescale: Self::default_timescale(),
			duration_max: None,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn defaults_timescale_to_ms() {
		let json = r#"{"track":"timeline.z","durationMax":2000}"#;
		let decoded: Timeline = serde_json::from_str(json).unwrap();
		assert_eq!(decoded.track, "timeline.z");
		assert_eq!(decoded.timescale, 1000);
		assert_eq!(decoded.duration_max, Some(2000));
	}

	#[test]
	fn duration_max_is_optional() {
		let json = r#"{"track":"timeline.z"}"#;
		let decoded: Timeline = serde_json::from_str(json).unwrap();
		assert_eq!(decoded.duration_max, None);
		assert_eq!(
			serde_json::to_string(&decoded).unwrap(),
			r#"{"track":"timeline.z","timescale":1000}"#
		);
	}

	#[test]
	fn zero_timescale_is_refused() {
		serde_json::from_str::<Timeline>(r#"{"track":"timeline.z","timescale":0}"#)
			.expect_err("a zero timescale must not decode");
	}

	#[test]
	fn explicit_null_timescale_is_refused() {
		serde_json::from_str::<Timeline>(r#"{"track":"timeline.z","timescale":null}"#)
			.expect_err("an explicit null timescale must not decode as the default");
	}

	#[test]
	fn roundtrip() {
		let mut timeline = Timeline::new("timeline.z");
		timeline.duration_max = Some(2000);
		let json = serde_json::to_string(&timeline).unwrap();
		assert_eq!(json, r#"{"track":"timeline.z","timescale":1000,"durationMax":2000}"#);
		assert_eq!(serde_json::from_str::<Timeline>(&json).unwrap(), timeline);
	}
}
