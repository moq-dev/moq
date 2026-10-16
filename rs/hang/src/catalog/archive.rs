use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds.
pub const MOQ_EPOCH_UNIX_MILLIS: u64 = 1_577_836_800_000;

/// One indexed track's timeline and nominal record duration.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Timeline {
	/// The MoQ track carrying this index's records.
	pub track: String,

	/// The nominal record duration in the archive's timescale, zero for sparse data, or `None` for
	/// an untimed track, whose records carry no duration.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration: Option<u64>,
}

impl Timeline {
	/// Declare `track` with its nominal `duration` in the archive timescale, `None` when untimed.
	pub fn new(track: impl Into<String>, duration: impl Into<Option<u64>>) -> Self {
		Self {
			track: track.into(),
			duration: duration.into(),
		}
	}
}

/// Discovers each track's timeline and any durable archive.
///
/// A live publisher sets the timeline fields (`timelines`, `timescale`) alone,
/// and a recording also names the replay broadcast and object store those spans live under.
/// Every advertised span is FETCHable; with a store they are durable.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Archive {
	/// Each indexed track's timeline declaration, keyed by the indexed track's name.
	pub timelines: BTreeMap<String, Timeline>,

	/// Units per second for every timeline's timestamps. Defaults to milliseconds.
	#[serde(
		default = "Archive::default_timescale",
		deserialize_with = "super::deserialize_timescale_or_default"
	)]
	pub timescale: u32,

	/// The MoQ broadcast the archive is served back from, relative to this catalog, if any.
	#[serde(default)]
	pub replay: Option<moq_net::path::RelativeOwned>,

	/// The object-store URL the recording objects live under, if exposed by the publisher.
	#[serde(default)]
	pub store: Option<url::Url>,

	/// The recording object format version, [`VERSION`](Self::VERSION) when a store is present.
	#[serde(default)]
	pub version: Option<u32>,
}

impl Archive {
	/// The recording object format advertised in [`version`](Self::version).
	pub const VERSION: u32 = 2;

	/// The default timeline timescale, milliseconds.
	pub const fn default_timescale() -> u32 {
		1000
	}

	/// An archive with no timelines or durable storage.
	pub fn new() -> Self {
		Self {
			timelines: BTreeMap::new(),
			timescale: Self::default_timescale(),
			replay: None,
			store: None,
			version: None,
		}
	}
}

impl Default for Archive {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn live() -> Archive {
		let mut archive = Archive::new();
		archive
			.timelines
			.insert("video".to_string(), Timeline::new("video.timeline.z", 4_000));
		archive
	}

	#[test]
	fn live_publisher_is_timeline_fields_alone() {
		let json = serde_json::to_string(&live()).unwrap();
		assert_eq!(
			json,
			r#"{"timelines":{"video":{"track":"video.timeline.z","duration":4000}},"timescale":1000}"#
		);
		assert_eq!(serde_json::from_str::<Archive>(&json).unwrap(), live());
	}

	#[test]
	fn recording_roundtrips_replay_store_and_version() {
		let mut archive = live();
		archive.replay = Some(moq_net::path::RelativeOwned::new("./recordings/clip"));
		archive.store = Some("https://objects.example/rec/".parse().unwrap());
		archive.version = Some(Archive::VERSION);

		let json = serde_json::to_string(&archive).unwrap();
		assert_eq!(
			json,
			r#"{"timelines":{"video":{"track":"video.timeline.z","duration":4000}},"timescale":1000,"replay":"recordings/clip","store":"https://objects.example/rec/","version":2}"#
		);
		assert_eq!(serde_json::from_str::<Archive>(&json).unwrap(), archive);
	}

	#[test]
	fn an_untimed_timeline_declares_no_duration() {
		let mut archive = Archive::new();
		archive
			.timelines
			.insert("data".to_string(), Timeline::new("data.timeline.z", None));
		let json = serde_json::to_string(&archive).unwrap();
		assert_eq!(
			json,
			r#"{"timelines":{"data":{"track":"data.timeline.z"}},"timescale":1000}"#
		);
		assert_eq!(serde_json::from_str::<Archive>(&json).unwrap(), archive);
	}

	#[test]
	fn zero_timescale_is_refused() {
		serde_json::from_str::<Archive>(r#"{"timelines":{},"timescale":0}"#)
			.expect_err("a zero timescale must not decode");
	}

	#[test]
	fn invalid_store_url_is_refused() {
		serde_json::from_str::<Archive>(r#"{"timelines":{},"store":"not a url"}"#)
			.expect_err("an invalid store URL must not decode");
	}

	#[test]
	fn a_single_shared_timeline_is_refused() {
		serde_json::from_str::<Archive>(r#"{"track":"timeline.z"}"#).expect_err("the old shape names no timelines");
	}
}
