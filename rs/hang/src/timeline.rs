//! The timeline track: the broadcast's segment index, one record per aligned segment.
//!
//! MoQ groups carry only an opaque sequence number; the timestamps live inside the media
//! frames. The timeline track republishes the broadcast's segmentation as metadata: each
//! record describes one *segment*, a span of content time shared by every media track, and
//! maps it to the group ranges that carry it on each track. A consumer can answer "which
//! groups cover time T on track X" (and "where is the live edge") from a few bytes per
//! segment without downloading media. That is exactly what an HLS/DASH origin needs to render
//! playlists without touching media bytes, and the index a VOD player seeks with.
//!
//! ## Segments
//!
//! A segment covers the same span of content time on every track of the broadcast, which is
//! what HLS requires of switchable renditions. Segment boundaries land on video keyframes
//! (every group already opens on one), so a video track contributes whole groups to each
//! segment; an audio track packs however many of its shorter groups fall inside the span.
//! A record is published once the segment is *complete*: every participating track's groups
//! for the span are known, so the record is self-contained and immediately servable.
//!
//! There is one timeline per broadcast, advertised by the catalog's root
//! [`Timeline`](crate::catalog::Timeline) section (its `track` field names this track,
//! [`DEFAULT_NAME`](crate::timeline::DEFAULT_NAME) by convention). A broadcast that doesn't need aligned segments simply
//! doesn't publish one.
//!
//! On the wire the track is a `moq-json` *stream* (see `moq_json::stream`): a single group,
//! one DEFLATE-compressed record per frame, every record preserved in order. Like the catalog,
//! a record tolerates and preserves unknown fields: extend it by flattening a [`Record`](crate::timeline::Record) into
//! your own struct via [`RecordExt`](crate::timeline::RecordExt).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Result;

/// The conventional name for a broadcast's timeline track (the `.z` marks the
/// DEFLATE-compressed stream, like the catalog's `.json.z` sibling).
///
/// A publisher records the actual name in the catalog's root
/// [`Timeline::track`](crate::catalog::Timeline::track) field; a consumer reads it from the
/// catalog rather than assuming, so this is only a default.
pub const DEFAULT_NAME: &str = "timeline.z";

/// The application extension carried alongside a record's base fields.
///
/// Defaults to `()` (no extra fields). Set an application's own typed struct to add fields
/// (e.g. an ad-break marker); it is flattened into the record's JSON object, exactly like
/// [`Catalog`](crate::Catalog)'s extension. `()` is the base case.
pub trait RecordExt: serde::Serialize + serde::de::DeserializeOwned + Default + Clone + Send + Unpin + 'static {}
impl RecordExt for () {}

/// A contiguous run of groups a track contributes to a segment, `start` through `end`
/// inclusive.
///
/// A track's segment entry is an array of these: more than one range means the group
/// sequence is discontinuous inside the segment (groups that never existed, e.g. a gappy
/// source).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Range {
	/// The first group of the run, as used by FETCH/SUBSCRIBE on the media track.
	pub start: u64,

	/// The last group of the run, inclusive.
	pub end: u64,

	/// Whether the run's first group starts with a keyframe, i.e. whether a player can join or
	/// switch renditions at this range. Defaults to `true` (and is omitted from the JSON when
	/// true); a publisher sets `false` when a source resumes without one, so an exporter knows
	/// not to advertise the segment as independently decodable.
	#[serde(default = "Range::default_keyframe", skip_serializing_if = "Clone::clone")]
	pub keyframe: bool,
}

impl Range {
	fn default_keyframe() -> bool {
		true
	}

	/// A range covering groups `start` through `end` inclusive, starting on a keyframe.
	pub fn new(start: u64, end: u64) -> Self {
		Self {
			start,
			end,
			keyframe: true,
		}
	}
}

/// One timeline record: a complete aligned segment, mapping its span of content time to the
/// group ranges that carry it on each track.
///
/// Records are self-contained: `pts` and `duration` bound the span (no peeking at the next
/// record), and `tracks` names every participating media track's group ranges. A track absent
/// from `tracks` has no content for the span (a gap; HLS `EXT-X-GAP`). `pts`/`duration` are in
/// the timescale declared by the catalog's [`Timeline`](crate::catalog::Timeline) section
/// (default milliseconds). Extend with a typed [`RecordExt`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(
	rename_all = "camelCase",
	bound(serialize = "E: serde::Serialize", deserialize = "E: serde::de::DeserializeOwned")
)]
pub struct Record<E: RecordExt = ()> {
	/// The segment's number. Consecutive within a broadcast, so it anchors HLS
	/// `EXT-X-MEDIA-SEQUENCE`; explicit (rather than implied by record order) so a reader
	/// joining mid-stream or across a windowed recording keeps stable numbering.
	pub segment: u64,

	/// The segment's start, in the timeline's timescale.
	pub pts: u64,

	/// The segment's duration, in the timeline's timescale. The next record's `pts` is
	/// `pts + duration` unless content time itself jumped (a discontinuity).
	pub duration: u64,

	/// The group ranges each participating media track contributes to this segment, keyed by
	/// track name. A track absent from the map has no content in this span.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub tracks: BTreeMap<String, Vec<Range>>,

	/// The application extension, flattened into the record's JSON object (nothing for the
	/// default `()`). See [`RecordExt`].
	#[serde(flatten)]
	pub ext: E,
}

impl<E: RecordExt> Record<E> {
	/// A record with no tracks and the default (empty) extension; fill in
	/// [`tracks`](Self::tracks) afterward.
	pub fn new(segment: u64, pts: u64, duration: u64) -> Self {
		Self {
			segment,
			pts,
			duration,
			tracks: BTreeMap::new(),
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

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrip() {
		let mut record = Record::<()>::new(3, 84_000, 6_000);
		record.tracks.insert("video".to_string(), vec![Range::new(42, 42)]);
		record
			.tracks
			.insert("audio".to_string(), vec![Range::new(512, 540), Range::new(550, 560)]);

		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			concat!(
				r#"{"segment":3,"pts":84000,"duration":6000,"tracks":{"#,
				r#""audio":[{"start":512,"end":540},{"start":550,"end":560}],"#,
				r#""video":[{"start":42,"end":42}]}}"#
			)
		);
		assert_eq!(Record::<()>::from_slice(&json).unwrap(), record);
	}

	#[test]
	fn keyframe_false_roundtrips_and_true_is_omitted() {
		let mut range = Range::new(7, 9);
		range.keyframe = false;
		let mut record = Record::<()>::new(0, 0, 2_000);
		record.tracks.insert("video".to_string(), vec![range]);

		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"segment":0,"pts":0,"duration":2000,"tracks":{"video":[{"start":7,"end":9,"keyframe":false}]}}"#
		);

		let decoded = Record::<()>::from_slice(&json).unwrap();
		assert_eq!(decoded, record);
		// An omitted flag decodes as the keyframe default.
		let plain: Record =
			Record::from_slice(br#"{"segment":0,"pts":0,"duration":1,"tracks":{"v":[{"start":1,"end":1}]}}"#).unwrap();
		assert!(plain.tracks["v"][0].keyframe);
	}

	#[test]
	fn empty_tracks_is_omitted() {
		let record = Record::<()>::new(1, 1_000, 500);
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"segment":1,"pts":1000,"duration":500}"#
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
			pts: 14_000,
			duration: 2_000,
			tracks: BTreeMap::new(),
			ext: Ext { discontinuity: true },
		};
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"segment":2,"pts":14000,"duration":2000,"discontinuity":true}"#
		);
		assert_eq!(Record::<Ext>::from_slice(&json).unwrap(), record);
	}
}
