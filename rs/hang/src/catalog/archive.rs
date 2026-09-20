use serde::{Deserialize, Serialize};

/// The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds.
pub const MOQ_EPOCH_UNIX_MILLIS: u64 = 1_577_836_800_000;

/// Discovers the broadcast's segment index and any durable archive.
///
/// A live publisher sets the timeline fields (`track`, `timescale`, `duration_max`) alone, and a
/// recording also names the replay broadcast and object store those ranges live under. Every
/// advertised range is FETCHable; with a store they are durable.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Archive {
	/// The MoQ track carrying the broadcast's segment records.
	pub track: String,

	/// Units per second for the timeline's timestamps. Defaults to milliseconds.
	#[serde(
		default = "Archive::default_timescale",
		deserialize_with = "super::deserialize_timescale_or_default"
	)]
	pub timescale: u32,

	/// The declared upper bound on a segment's duration, in [`timescale`](Self::timescale) units.
	pub duration_max: Option<u64>,

	/// The MoQ broadcast the archive is served back from, relative to this catalog, if any.
	#[serde(default)]
	pub replay: Option<moq_net::PathRelativeOwned>,

	/// The object-store URL the recording objects live under, if exposed by the publisher.
	#[serde(default)]
	pub store: Option<url::Url>,

	/// The recording object format version, [`VERSION`](Self::VERSION) when a store is present.
	#[serde(default)]
	pub version: Option<u32>,
}

impl Archive {
	/// The recording object format advertised in [`version`](Self::version).
	pub const VERSION: u32 = 1;

	/// The default timeline timescale, milliseconds.
	pub const fn default_timescale() -> u32 {
		1000
	}

	/// An archive naming `track` as its timeline, with no duration bound or durable storage.
	pub fn new(track: impl Into<String>) -> Self {
		Self {
			track: track.into(),
			timescale: Self::default_timescale(),
			duration_max: None,
			replay: None,
			store: None,
			version: None,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn live_publisher_is_timeline_fields_alone() {
		let json = serde_json::to_string(&Archive::new("timeline.z")).unwrap();
		assert_eq!(json, r#"{"track":"timeline.z","timescale":1000}"#);
		assert_eq!(
			serde_json::from_str::<Archive>(&json).unwrap(),
			Archive::new("timeline.z")
		);
	}

	#[test]
	fn recording_roundtrips_replay_store_and_version() {
		let mut archive = Archive::new("timeline.z");
		archive.replay = Some(moq_net::PathRelativeOwned::new("./recordings/clip"));
		archive.store = Some("https://objects.example/rec/".parse().unwrap());
		archive.version = Some(Archive::VERSION);

		let json = serde_json::to_string(&archive).unwrap();
		assert_eq!(
			json,
			r#"{"track":"timeline.z","timescale":1000,"replay":"recordings/clip","store":"https://objects.example/rec/","version":1}"#
		);
		assert_eq!(serde_json::from_str::<Archive>(&json).unwrap(), archive);
	}

	#[test]
	fn zero_timescale_is_refused() {
		serde_json::from_str::<Archive>(r#"{"track":"timeline.z","timescale":0}"#)
			.expect_err("a zero timescale must not decode");
	}

	#[test]
	fn invalid_store_url_is_refused() {
		serde_json::from_str::<Archive>(r#"{"track":"timeline.z","store":"not a url"}"#)
			.expect_err("an invalid store URL must not decode");
	}
}
