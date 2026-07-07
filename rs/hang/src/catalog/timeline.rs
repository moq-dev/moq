use serde::{Deserialize, Serialize};

/// The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds.
///
/// Timeline [`wall`](Timeline::wall) values are measured from here rather than the Unix epoch so
/// the numbers stay small (and safely within a 53-bit integer even at fine timescales); a
/// consumer recovers Unix time by adding this back.
pub const MOQ_EPOCH_UNIX_MILLIS: u64 = 1_577_836_800_000;

/// Describes a media track's companion timeline track.
///
/// A timeline track maps each of the media track's groups to its start timestamp (see the
/// [`timeline`](crate::timeline) module for the record format), so a consumer can seek, or
/// build an HLS/DASH playlist, without downloading the media itself. This section, present on
/// a [`VideoConfig`](crate::catalog::VideoConfig) or
/// [`AudioConfig`](crate::catalog::AudioConfig) when the publisher offers one, points at that
/// track and declares how to read its timestamps.
///
/// The section is per media track on purpose: audio and video groups have different
/// durations (and metadata tracks different still), so a single broadcast-wide timeline can't
/// describe them all. Each media track carries its own.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Timeline {
	/// The name of the companion MoQ track carrying this track's group -> timestamp records.
	pub track: String,

	/// Units per second for the timeline's `pts` and [`wall`](Self::wall). Defaults to 1000
	/// (milliseconds).
	#[serde(default = "Timeline::default_timescale")]
	pub timescale: u32,

	/// The wall-clock time of `pts` 0, in [`timescale`](Self::timescale) units since the moq
	/// epoch ([`MOQ_EPOCH_UNIX_MILLIS`], 2020-01-01), if known. A consumer derives the wall-clock
	/// time of any group as `wall + pts`, and Unix time by adding the moq epoch back (an absolute
	/// clock for HLS `EXT-X-PROGRAM-DATE-TIME` / DASH `availabilityStartTime`).
	///
	/// Measured from 2020 rather than 1970 so the value stays small and safely within a 53-bit
	/// integer even at fine timescales.
	pub wall: Option<u64>,
}

impl Timeline {
	/// The default timescale (1000, i.e. milliseconds) for a timeline whose catalog section
	/// omits the field.
	pub fn default_timescale() -> u32 {
		1000
	}

	/// A timeline section naming `track`, at the default millisecond timescale and with no
	/// wall-clock anchor. Set [`timescale`](Self::timescale) / [`wall`](Self::wall) afterward.
	pub fn new(track: impl Into<String>) -> Self {
		Self {
			track: track.into(),
			timescale: Self::default_timescale(),
			wall: None,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn defaults_timescale_to_ms() {
		let json = r#"{"track":"video0.timeline"}"#;
		let decoded: Timeline = serde_json::from_str(json).unwrap();
		assert_eq!(decoded.track, "video0.timeline");
		assert_eq!(decoded.timescale, 1000);
		assert_eq!(decoded.wall, None);
	}

	#[test]
	fn roundtrip_with_wall() {
		let mut timeline = Timeline::new("audio0.timeline");
		timeline.wall = Some(1_751_846_400_000);
		let json = serde_json::to_string(&timeline).unwrap();
		assert_eq!(
			json,
			r#"{"track":"audio0.timeline","timescale":1000,"wall":1751846400000}"#
		);
		assert_eq!(serde_json::from_str::<Timeline>(&json).unwrap(), timeline);
	}
}
