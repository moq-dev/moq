//! The timeline track: an ordered log mapping each of a media track's groups to its start
//! timestamp.
//!
//! MoQ groups carry only an opaque sequence number; the timestamps live inside the media
//! frames. A timeline track republishes that mapping as metadata, so a consumer can answer
//! "which group covers time T" (and "where is the live edge") from a few bytes per group
//! instead of downloading the media itself. That is the primitive an HLS/DASH origin needs to
//! render playlists without touching media bytes, and the index a VOD player seeks with.
//!
//! A timeline is per media track (audio and video groups have different durations, so they
//! can't share one). The media track's catalog entry carries a
//! [`Timeline`](crate::catalog::Timeline) section naming the companion timeline track and its
//! timescale (`pts` is in those units, default milliseconds) and optional wall-clock anchor.
//! This module is just the wire record on that track.
//!
//! On the wire the track is a `moq-json` *stream* (see `moq_json::stream`): a single group, one
//! DEFLATE-compressed record per frame. The record shape follows the `mediatimeline` concept
//! from [draft-ietf-moq-msf](https://datatracker.ietf.org/doc/draft-ietf-moq-msf/), with JSON
//! keys instead of positional arrays (DEFLATE absorbs the repeated keys) and a moq-lite group
//! sequence instead of a `[group, object]` location (moq-lite has no object IDs).
//!
//! Like the catalog, a record tolerates and preserves unknown fields: extend it by flattening
//! a `Record` into your own struct, or read its `ext` field directly.

use serde::{Deserialize, Serialize};

use crate::Result;

/// The application extension carried alongside a record's `group`/`pts`.
///
/// Defaults to `()` (no extra fields). Set an application's own typed struct to add fields
/// (e.g. a discontinuity flag, a measured bitrate); it is flattened into the record's JSON
/// object, exactly like [`Catalog`](crate::Catalog)'s extension. `()` is the base case.
pub trait RecordExt: serde::Serialize + serde::de::DeserializeOwned + Default + Clone + Send + Unpin + 'static {}
impl RecordExt for () {}

/// One timeline record: the media track opened group `group` at presentation time `pts`.
///
/// Records are appended when the media group opens, so the live edge of the timeline is the
/// live edge of the broadcast. A group's duration is implicit: the gap to the next record.
/// `pts` is in the timescale declared by the track's [`Timeline`](crate::catalog::Timeline)
/// catalog section (default milliseconds). Extend it with a typed [`RecordExt`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(
	rename_all = "camelCase",
	bound(serialize = "E: serde::Serialize", deserialize = "E: serde::de::DeserializeOwned")
)]
pub struct Record<E: RecordExt = ()> {
	/// The group's sequence number, as used by FETCH/SUBSCRIBE on the media track.
	pub group: u64,

	/// The group's start (its first frame's presentation timestamp), in the timeline's
	/// timescale.
	pub pts: u64,

	/// The application extension, flattened into the record's JSON object (nothing for the
	/// default `()`). See [`RecordExt`].
	#[serde(flatten)]
	pub ext: E,
}

impl<E: RecordExt> Record<E> {
	/// A record with the default (empty) extension.
	pub fn new(group: u64, pts: u64) -> Self {
		Self {
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
		let record = Record::<()>::new(42, 84_000);
		let json = record.to_vec().unwrap();
		assert_eq!(std::str::from_utf8(&json).unwrap(), r#"{"group":42,"pts":84000}"#);
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
			group: 7,
			pts: 14_000,
			ext: Ext { discontinuity: true },
		};
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"group":7,"pts":14000,"discontinuity":true}"#
		);
		assert_eq!(Record::<Ext>::from_slice(&json).unwrap(), record);
	}

	#[test]
	fn conventional_track_name() {
		assert_eq!(track_name("video0"), "video0.timeline.z");
	}
}
