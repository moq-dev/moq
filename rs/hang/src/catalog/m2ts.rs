use serde::{Deserialize, Serialize};

/// A transport stream carried whole, as one track of verbatim packets.
///
/// The track's frames are runs of whole 188-byte packets in source order, each in the
/// [`Legacy`](crate::catalog::Container::Legacy) container with the PCR time of its first byte.
/// Nothing about the multiplex is described here because all of it stays in-band: the PAT, PMT,
/// and SI ride the track as authored, scrambled payloads included. A demultiplexed broadcast
/// describes each elementary stream as a rendition instead; a track is one shape or the other.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without bumping the major
/// version. External callers build one with [`M2ts::new`] and assign the fields they need.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct M2ts {
	/// The track carrying the packets, published in the same broadcast as the catalog.
	pub track: String,

	/// Whether every group starts at a random access point of the multiplex's one video stream,
	/// so a subscriber can decode from the first object of any group.
	///
	/// Only ever true for a single program with at most one video stream: programs or video
	/// streams sharing a clock can stagger their GOPs, leaving no packet that starts all of them.
	/// Always written; a reader treats it as false when absent.
	#[serde(default)]
	pub random_access: bool,

	/// The rate the source's PCR paced the whole multiplex at, in bits per second, null packets
	/// included. Present only while the source holds a constant rate.
	#[serde(default)]
	pub mux_rate: Option<u64>,
}

impl M2ts {
	/// A section naming `track`, with no random access promised and no rate recorded.
	pub fn new(track: impl Into<String>) -> Self {
		Self {
			track: track.into(),
			random_access: false,
			mux_rate: None,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrip() {
		let mut section = M2ts::new("0.m2ts");
		section.random_access = true;
		section.mux_rate = Some(2_499_999);

		let json = serde_json::to_string(&section).unwrap();
		assert_eq!(json, r#"{"track":"0.m2ts","randomAccess":true,"muxRate":2499999}"#);
		assert_eq!(serde_json::from_str::<M2ts>(&json).unwrap(), section);
	}

	#[test]
	fn random_access_is_always_written_and_mux_rate_only_when_known() {
		let json = serde_json::to_string(&M2ts::new("0.m2ts")).unwrap();
		assert_eq!(json, r#"{"track":"0.m2ts","randomAccess":false}"#);
	}

	#[test]
	fn absent_random_access_reads_as_false() {
		let section: M2ts = serde_json::from_str(r#"{"track":"0.m2ts"}"#).unwrap();
		assert_eq!(section, M2ts::new("0.m2ts"));
	}

	#[test]
	fn unknown_fields_are_ignored() {
		let section: M2ts = serde_json::from_str(r#"{"track":"0.m2ts","packetSize":188}"#).unwrap();
		assert_eq!(section.track, "0.m2ts");
	}

	#[test]
	fn a_section_without_a_track_is_refused() {
		serde_json::from_str::<M2ts>(r#"{"randomAccess":true}"#).expect_err("a section must name its track");
	}
}
