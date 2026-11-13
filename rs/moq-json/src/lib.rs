//! JSON publishing over [`moq-net`](moq_net) tracks, in three modes:
//!
//! - [`snapshot`]: **lossy**. One JSON value updated over time; `next` yields states in order,
//!   while `latest` skips to the newest state. Superseded groups are lost.
//! - [`stream`]: **lossless**. An ordered append-log of self-contained records; every record is
//!   preserved and delivered in order, nothing is ever superseded.
//! - [`window`]: **bounded**. An ordered run of records appended to the back and dropped from the
//!   front, which a reader can join at any point.
//!
//! Pick [`snapshot`] when consumers care about "what is the value now" (a catalog, a status
//! document), [`stream`] when they care about every record of an unbounded log, and [`window`] when
//! the publisher retires old records and a late reader should start from what is still retained.
//!
//! Each mode comes in two layers. `Producer`/`Consumer` own a [`moq_net`] track and manage its
//! groups. `Encoder`/`Decoder` are the same logic without the track: values in, frame payloads out
//! (and back), with the encoder saying where the group boundaries fall. Reach for the codec layer
//! when something else already owns the track, such as a `moq_mux::container::Producer` also
//! managing a timeline and a catalog estimate.
//!
//! The `poll_*` methods are always available. Enable the `async` feature for their generated
//! async wrappers (`next`, `latest`, and `refresh`); it is off by default.

#![warn(missing_docs)]

mod diff;
mod merge;
mod refresh;
pub mod snapshot;
mod state;
pub mod stream;
mod strict;
pub mod window;

/// Test support for the codec bench, enabled by the `bench` feature: the merge-patch diff.
#[cfg(feature = "bench")]
pub mod bench {
	pub use crate::diff::{Diff, diff};
}

/// The most a frame, or a group's frames together, may hold: 32 MiB, which moq-net's group cache
/// also holds, so a larger frame is refused before it is written.
pub(crate) const MAX_GROUP_BYTES: u64 = 32 * 1024 * 1024;

/// Where a group whose first frame is at `timestamp` starts on `track`.
///
/// A timed group starts at its first frame's timestamp, which the caller already rescaled to the
/// track. A scale-less frame carries none, so its group starts one past the newest start the track
/// holds: that stands in until every write carries a timestamp.
pub(crate) fn group_start(track: &moq_net::track::Producer, timestamp: Option<moq_net::Timestamp>) -> u64 {
	match timestamp {
		Some(timestamp) => timestamp.value(),
		None => track.largest().map_or(0, |largest| largest.group + 1),
	}
}

/// Whether a group whose first frame is at `timestamp` would start past every group on `track`,
/// which a new group must: two groups never share a start.
pub(crate) fn starts_past(track: &moq_net::track::Producer, timestamp: Option<moq_net::Timestamp>) -> bool {
	let start = group_start(track, timestamp);
	track.largest().is_none_or(|largest| start > largest.group)
}

/// Errors produced while publishing or consuming JSON.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying track.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// A value failed to serialize, deserialize, or apply as a merge patch.
	///
	/// Stored as a string since [`serde_json::Error`] is not [`Clone`].
	#[error("json: {0}")]
	Json(String),

	/// A merge patch arrived with no snapshot to apply it to.
	///
	/// Every group opens with a full snapshot, so this means frames reached
	/// [`snapshot::Decoder`] out of order, or a group's first frame was routed as a delta.
	#[error("delta before snapshot")]
	MissingSnapshot,

	/// A [`stream`] track carried a second group, which a lossless log cannot do.
	///
	/// A stream is a single group by construction: a publisher that cannot write a record ends the
	/// track rather than rolling. A second group therefore means the records that would have
	/// completed the first one are gone, so the read fails instead of presenting the remainder as
	/// a continuous log.
	#[error("stream rolled to a second group")]
	Rolled,

	/// A decoded frame or reconstructed snapshot exceeded its configured byte budget.
	#[error("decoded JSON exceeded {0} bytes")]
	TooLarge(usize),

	/// A decoded frame repeated a key within one JSON object, which parsers resolve differently.
	#[error("duplicate JSON object key")]
	DuplicateKey,
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
