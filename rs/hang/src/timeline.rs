//! Per-track timelines: each track's index of stored spans, one record per span.
//!
//! MoQ groups carry only an opaque sequence number; the timestamps live inside the media
//! frames. A timeline track republishes one track's spans as metadata: each record maps a span
//! of content time to the group and frame positions that carry it. A consumer can answer
//! "which groups cover time T on track X" (and "where is the live edge") from a few bytes per
//! span without downloading media. That is what an HLS/DASH origin needs to render playlists
//! without touching media bytes, the index a VOD player seeks with, and the object index of a
//! recording.
//!
//! ## Spans
//!
//! Every track has its own timeline, so tracks cut, commit, and expire independently. A span
//! usually holds whole groups; a group that outlives the publisher's maximum span is split by
//! frame, so an append-only group that never closes is still indexed as its frames arrive.
//! Spans are contiguous in position: a record starts where the previous one ended unless the
//! track skipped group sequences in between.
//!
//! The catalog's root [`Archive`](crate::catalog::Archive) entry maps each track to its
//! timeline track, conventionally the track name plus [`SUFFIX`](crate::timeline::SUFFIX).
//!
//! On the wire each timeline is a DEFLATE-compressed `moq-json` window (see `moq_json::window`).
//! Each group starts with a checkpoint and continues with push/pop operations; group rolls are an
//! encoding detail that consumers do not surface as duplicate records. Like the catalog, a record
//! tolerates and preserves unknown fields: extend it by flattening a
//! [`Record`](crate::timeline::Record) into your own struct via
//! [`RecordExt`](crate::timeline::RecordExt).

use serde::{Deserialize, Serialize};

use crate::Result;

/// The conventional suffix appended to a track name to name its timeline track (the `.z` marks
/// the DEFLATE-compressed stream, like the catalog's `.json.z` sibling).
///
/// A publisher records the actual names in the catalog's root
/// [`Archive`](crate::catalog::Archive) entry; a consumer reads them from the catalog rather than
/// assuming, so this is only a default.
pub const SUFFIX: &str = ".timeline.z";

/// The conventional timeline track name for `track`: the name plus [`SUFFIX`].
pub fn default_name(track: &str) -> String {
	format!("{track}{SUFFIX}")
}

/// The application extension carried alongside a record's base fields.
///
/// Defaults to `()` (no extra fields). Set an application's own typed struct to add fields
/// (e.g. an ad-break marker); it is flattened into the record's JSON object, exactly like
/// [`Catalog`](crate::Catalog)'s extension. `()` is the base case.
pub trait RecordExt: serde::Serialize + serde::de::DeserializeOwned + Default + Clone + Send + Unpin + 'static {}
impl RecordExt for () {}

/// A frame position within a track: frame `frame` of group `group`.
///
/// Positions order by group, then frame. `frame` is omitted from the JSON when zero, so a
/// position at a group start reads as just the group.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
	/// The group sequence, as used by FETCH/SUBSCRIBE on the track.
	pub group: u64,

	/// The frame index within the group.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub frame: u64,
}

fn is_zero(v: &u64) -> bool {
	*v == 0
}

impl Position {
	/// Frame `frame` of group `group`.
	pub const fn new(group: u64, frame: u64) -> Self {
		Self { group, frame }
	}

	/// The start of group `group`.
	pub const fn group(group: u64) -> Self {
		Self { group, frame: 0 }
	}
}

/// One timeline record: a span of one track, mapping its content time to frame positions.
///
/// The span holds every frame from [`start`](Self::start) (inclusive) to [`end`](Self::end)
/// (exclusive). An `end` at frame zero of group `g` means the span holds every frame of group
/// `g - 1` and nothing of `g`, whether or not `g` exists. `pts`/`duration` are in the timescale
/// declared by the catalog's [`Archive`](crate::catalog::Archive) entry (default milliseconds).
/// Extend with a typed [`RecordExt`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(
	rename_all = "camelCase",
	bound(serialize = "E: serde::Serialize", deserialize = "E: serde::de::DeserializeOwned")
)]
pub struct Record<E: RecordExt = ()> {
	/// The record's number, consecutive within its track's timeline and equal to its window index.
	pub sequence: u64,

	/// The span's start, the timestamp of its first frame, in the timeline's timescale.
	pub pts: u64,

	/// The span's duration, in the timeline's timescale. The next record's `pts` is
	/// `pts + duration` unless content time itself jumped (a discontinuity).
	pub duration: u64,

	/// The first frame of the span, inclusive.
	pub start: Position,

	/// The end of the span, exclusive.
	pub end: Position,

	/// Whether the span's first frame is a keyframe, i.e. whether a player can join or switch
	/// renditions at this record. Defaults to `true` (and is omitted from the JSON when true).
	#[serde(default = "default_keyframe", skip_serializing_if = "Clone::clone")]
	pub keyframe: bool,

	/// The application extension, flattened into the record's JSON object (nothing for the
	/// default `()`). See [`RecordExt`].
	#[serde(flatten)]
	pub ext: E,
}

fn default_keyframe() -> bool {
	true
}

impl<E: RecordExt> Record<E> {
	/// A keyframe record spanning `start..end` with the default (empty) extension.
	pub fn new(sequence: u64, pts: u64, duration: u64, start: Position, end: Position) -> Self {
		Self {
			sequence,
			pts,
			duration,
			start,
			end,
			keyframe: true,
			ext: E::default(),
		}
	}

	/// The groups holding at least one frame of the span, in order.
	pub fn groups(&self) -> std::ops::RangeInclusive<u64> {
		let last = match self.end.frame {
			0 => self.end.group.saturating_sub(1),
			_ => self.end.group,
		};
		self.start.group..=last
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
		let record = Record::<()>::new(3, 84_000, 6_000, Position::group(42), Position::group(44));
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"sequence":3,"pts":84000,"duration":6000,"start":{"group":42},"end":{"group":44}}"#
		);
		assert_eq!(Record::<()>::from_slice(&json).unwrap(), record);
		assert_eq!(record.groups(), 42..=43);
	}

	#[test]
	fn a_frame_split_roundtrips() {
		let mut record = Record::<()>::new(0, 0, 10_000, Position::new(7, 300), Position::new(7, 600));
		record.keyframe = false;
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"sequence":0,"pts":0,"duration":10000,"start":{"group":7,"frame":300},"end":{"group":7,"frame":600},"keyframe":false}"#
		);
		assert_eq!(Record::<()>::from_slice(&json).unwrap(), record);
		assert_eq!(record.groups(), 7..=7);

		// An omitted flag decodes as the keyframe default.
		let plain: Record =
			Record::from_slice(br#"{"sequence":0,"pts":0,"duration":1,"start":{"group":1},"end":{"group":2}}"#)
				.unwrap();
		assert!(plain.keyframe);
	}

	#[test]
	fn default_name_appends_the_suffix() {
		assert_eq!(default_name("video0"), "video0.timeline.z");
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

		let mut record = Record::<Ext>::new(2, 14_000, 2_000, Position::group(1), Position::group(2));
		record.ext.discontinuity = true;
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"sequence":2,"pts":14000,"duration":2000,"start":{"group":1},"end":{"group":2},"discontinuity":true}"#
		);
		assert_eq!(Record::<Ext>::from_slice(&json).unwrap(), record);
	}
}
