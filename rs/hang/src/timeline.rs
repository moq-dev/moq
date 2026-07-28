//! The timeline track: an ordered log of a media track's segments, each one or more groups.
//!
//! MoQ groups carry only an opaque sequence number; the timestamps live inside the media
//! frames. A timeline track republishes a segment index as metadata: each record says "segment
//! N of this track starts at group G, at presentation time T". A consumer can answer "which
//! groups cover time T" (and "where is the live edge") from a few bytes per segment instead of
//! downloading the media itself. That is exactly the information an HLS/DASH origin needs to
//! render playlists without touching media bytes, and the index a VOD player seeks with.
//!
//! ## Segments
//!
//! A segment is one or more consecutive groups of a track: it spans from its record's `group`
//! up to (excluding) the next record's `group`, and its duration is the gap to the next
//! record's `pts`. Segment numbers are *aligned across the tracks of a broadcast*: segment 5
//! of the audio timeline covers the same span of content time as segment 5 of the video
//! timeline. Alignment at group granularity is what makes the timelines sufficient for
//! HLS/DASH export, where renditions must cut at the same boundaries to be switchable.
//!
//! The tracks still differ in how many groups a segment holds. Video groups open on keyframes,
//! and a keyframe is where a segment must start, so a video segment is exactly one group.
//! Audio groups are much shorter, so an audio segment packs every audio group whose start
//! falls inside the segment's span (the record names the first; the rest follow contiguously
//! until the next record's group).
//!
//! A timeline is still per media track: each track's catalog entry carries a
//! [`Timeline`](crate::catalog::Timeline) section naming its companion timeline track, the
//! timescale its `pts` values use (default milliseconds), and an optional wall-clock anchor.
//! Only the segment *numbering* is shared.
//!
//! On the wire the track is a `moq-json` *stream* (see `moq_json::stream`): a single group, one
//! DEFLATE-compressed record per frame. Like the catalog, a record tolerates and preserves
//! unknown fields: extend it by flattening a `Record` into your own struct, or read its `ext`
//! field directly.

use serde::{Deserialize, Serialize};

use crate::Result;

/// The application extension carried alongside a record's `segment`/`group`/`pts`.
///
/// Defaults to `()` (no extra fields). Set an application's own typed struct to add fields
/// (e.g. a discontinuity flag, a measured bitrate); it is flattened into the record's JSON
/// object, exactly like [`Catalog`](crate::Catalog)'s extension. `()` is the base case.
pub trait RecordExt: serde::Serialize + serde::de::DeserializeOwned + Default + Clone + Send + Unpin + 'static {}
impl RecordExt for () {}

/// One timeline record: segment `segment` of the media track starts at group `group`, at
/// presentation time `pts`.
///
/// Records are appended when the segment's first group opens, so the live edge of the timeline
/// is the live edge of the broadcast. A segment's group span and duration are implicit: it runs
/// until the next record's `group` and `pts`. Segment numbers are aligned across the broadcast's
/// tracks (see the [module docs](self)); `pts` is in the timescale declared by the track's
/// [`Timeline`](crate::catalog::Timeline) catalog section (default milliseconds). Extend it with
/// a typed [`RecordExt`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(
	rename_all = "camelCase",
	bound(serialize = "E: serde::Serialize", deserialize = "E: serde::de::DeserializeOwned")
)]
pub struct Record<E: RecordExt = ()> {
	/// The segment's number, aligned across the broadcast's tracks.
	pub segment: u64,

	/// The segment's first group, as used by FETCH/SUBSCRIBE on the media track. The segment
	/// covers every group up to (excluding) the next record's `group`.
	pub group: u64,

	/// The segment's start (its first frame's presentation timestamp), in the timeline's
	/// timescale.
	pub pts: u64,

	/// The application extension, flattened into the record's JSON object (nothing for the
	/// default `()`). See [`RecordExt`].
	#[serde(flatten)]
	pub ext: E,
}

impl<E: RecordExt> Record<E> {
	/// A record with the default (empty) extension.
	pub fn new(segment: u64, group: u64, pts: u64) -> Self {
		Self {
			segment,
			group,
			pts,
			ext: E::default(),
		}
	}

	/// Parse a record from a slice of bytes.
	pub fn from_slice(v: &[u8]) -> Result<Self> {
		Ok(serde_json::from_slice(v)?)
	}

	/// Serialize the record to a vector of bytes.
	pub fn to_vec(&self) -> Result<Vec<u8>> {
		Ok(serde_json::to_vec(self)?)
	}
}

/// The conventional companion timeline track name for a media rendition: `<rendition>.timeline.z`
/// (the `.z` marks the DEFLATE-compressed stream, like the catalog's `.json.z` sibling).
///
/// A publisher names the timeline track this way and records it in the media track's
/// [`Timeline::track`](crate::catalog::Timeline::track) catalog field; a consumer reads the
/// name from the catalog rather than reconstructing it, so this is only a default.
pub fn track_name(rendition: &str) -> String {
	format!("{rendition}.timeline.z")
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrip() {
		let record = Record::<()>::new(3, 42, 84_000);
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"segment":3,"group":42,"pts":84000}"#
		);
		assert_eq!(Record::<()>::from_slice(&json).unwrap(), record);
	}

	#[test]
	fn typed_extension_flattens() {
		// An application extends the record with its own typed section, flattened into the object.
		#[derive(serde::Serialize, serde::Deserialize, Default, Clone, PartialEq, Debug)]
		struct Ext {
			#[serde(skip_serializing_if = "std::ops::Not::not", default)]
			discontinuity: bool,
		}
		impl RecordExt for Ext {}

		let record = Record {
			segment: 2,
			group: 7,
			pts: 14_000,
			ext: Ext { discontinuity: true },
		};
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"segment":2,"group":7,"pts":14000,"discontinuity":true}"#
		);
		assert_eq!(Record::<Ext>::from_slice(&json).unwrap(), record);
	}

	#[test]
	fn conventional_track_name() {
		assert_eq!(track_name("video0"), "video0.timeline.z");
	}
}
