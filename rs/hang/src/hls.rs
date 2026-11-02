//! HLS segment records: where each of a rendition's HLS segments starts.
//!
//! A rendition advertises them with its catalog [`hls`](crate::catalog::Hls) field, which names a track
//! carrying one [`Record`](crate::hls::Record) per segment. A segment starts at a group boundary, so a record is the
//! segment's first group and pts. It runs to the next record: it holds every group up to the next
//! record's `group` and lasts until the next record's `pts`, unless it carries an explicit
//! `duration`. An edge renders HLS (or DASH) playlists from the records without touching media.
//!
//! A zero `duration` marks where content stopped, at a break or the end of the track: that segment
//! holds no media, and what follows it, if anything, is a discontinuity. Records publish when their
//! segment starts, so a segment is complete once the next record, or its own duration, says where
//! it ends.
//!
//! Every `pts` and `duration` is in milliseconds. On the wire the track is a compressed
//! `moq-json` window (see `moq_json::window`): each group starts with a checkpoint and continues
//! with push and pop operations.

use serde::{Deserialize, Serialize};

use crate::Result;

/// The conventional suffix appended to a media track's name to name its HLS record track.
///
/// A consumer reads the actual name from the rendition's [`hls`](crate::catalog::Hls) field rather
/// than assuming it.
pub const SUFFIX: &str = ".hls";

/// The conventional HLS record track name for `track`: the name plus [`SUFFIX`].
pub fn default_name(track: &str) -> String {
	format!("{track}{SUFFIX}")
}

/// One HLS segment: its first group and pts, in milliseconds.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Record {
	/// The segment's number, consecutive within the track and equal to the record's window index.
	pub sequence: u64,

	/// The group the segment starts with.
	pub group: u64,

	/// The pts of the segment's first frame, in milliseconds.
	pub pts: u64,

	/// The segment's duration in milliseconds, when it does not run to the next record's pts. Zero
	/// marks where content stopped: the segment holds no media.
	#[serde(default)]
	pub duration: Option<u64>,
}

impl Record {
	/// Segment `sequence`, starting with `group` at `pts` milliseconds and running to the next record.
	pub fn new(sequence: u64, group: u64, pts: u64) -> Self {
		Self {
			sequence,
			group,
			pts,
			duration: None,
		}
	}

	/// Segment `sequence` lasting exactly `duration` milliseconds; zero marks where content stopped.
	pub fn with_duration(mut self, duration: u64) -> Self {
		self.duration = Some(duration);
		self
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
		let record = Record::new(3, 42, 84_000);
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"sequence":3,"group":42,"pts":84000}"#
		);
		assert_eq!(Record::from_slice(&json).unwrap(), record);
	}

	#[test]
	fn a_stop_carries_its_duration() {
		let record = Record::new(4, 44, 88_000).with_duration(0);
		let json = record.to_vec().unwrap();
		assert_eq!(
			std::str::from_utf8(&json).unwrap(),
			r#"{"sequence":4,"group":44,"pts":88000,"duration":0}"#
		);
		assert_eq!(Record::from_slice(&json).unwrap(), record);
	}

	#[test]
	fn a_record_without_its_group_is_refused() {
		assert!(Record::from_slice(br#"{"sequence":0,"pts":0}"#).is_err());
	}
}
