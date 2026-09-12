use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

use super::Timeline;

/// Discovers the broadcast's segment index and any durable archive.
///
/// This is the catalog's one name for the segment index: a live publisher sets
/// the flattened [`timeline`](Self::timeline) fields alone, and a recording also
/// names the replay broadcast and object store those ranges live under. Every
/// advertised range is FETCHable; with a store they are durable. There is no
/// sibling `timeline` entry.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Archive {
	/// The timeline track: its name, timescale, optional duration bound, and wall-clock
	/// anchor. Flattened on the wire so a live publisher's `archive` is those fields
	/// alone.
	#[serde(flatten)]
	pub timeline: Timeline,

	/// The MoQ broadcast the archive is served back from, relative to this catalog, if
	/// any.
	///
	/// Absent when the timeline lives on this broadcast. A wildcard replay path names
	/// no generation: compare this path and [`store`](Self::store) to tell recordings
	/// apart. Authorization is external.
	#[serde(default)]
	pub replay: Option<moq_net::PathRelativeOwned>,

	/// The object-store URL the recording objects live under, if the publisher exposes
	/// one. Authorization is external, so a managed store and a customer-owned one share
	/// this field.
	#[serde(default)]
	pub store: Option<url::Url>,

	/// The recording object format version, [`VERSION`](Self::VERSION) when this crate
	/// writes objects. Absent when there is no store.
	#[serde(default)]
	pub version: Option<u32>,
}

impl Archive {
	/// The recording object format advertised in [`version`](Self::version) when a store
	/// is present.
	pub const VERSION: u32 = 1;

	/// An archive naming `track` as its timeline, with no replay path, store, or format
	/// version. Set [`replay`](Self::replay) / [`store`](Self::store) /
	/// [`version`](Self::version) afterward.
	pub fn new(track: impl Into<String>) -> Self {
		Self {
			timeline: Timeline::new(track),
			replay: None,
			store: None,
			version: None,
		}
	}
}

impl From<Timeline> for Archive {
	fn from(timeline: Timeline) -> Self {
		Self {
			timeline,
			replay: None,
			store: None,
			version: None,
		}
	}
}

impl Deref for Archive {
	type Target = Timeline;

	fn deref(&self) -> &Timeline {
		&self.timeline
	}
}

impl DerefMut for Archive {
	fn deref_mut(&mut self) -> &mut Timeline {
		&mut self.timeline
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
	fn invalid_store_url_is_refused() {
		serde_json::from_str::<Archive>(r#"{"track":"timeline.z","store":"not a url"}"#)
			.expect_err("an invalid store URL must not decode");
	}
}
