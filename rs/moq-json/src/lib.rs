//! JSON publishing over [`moq-net`](moq_net) tracks, in two modes:
//!
//! - [`snapshot`]: **lossy**. One JSON value updated over time; a consumer only gets the most
//!   recent value. Intermediate updates are collapsed and older groups are dropped.
//! - [`stream`]: **lossless**. An ordered append-log of self-contained records; every record is
//!   preserved and delivered in order, nothing is ever superseded.
//! - [`window`]: **sliding**. An ordered log that appends at the tail and removes from the head,
//!   like an HLS playlist. Records carry stable positions and old groups are disposable.
//!
//! Pick [`snapshot`] when consumers care about "what is the value now" (a catalog, a status
//! document), [`stream`] when they care about every record ever written (an event log), and
//! [`window`] when records expire (a media timeline over a bounded cache).
//!
//! [`snapshot`] and [`stream`] each come in two layers. `Producer`/`Consumer` own a [`moq_net`]
//! track and manage its groups. `Encoder`/`Decoder` are the same logic without the track: values
//! in, frame payloads out (and back), with the encoder saying where the group boundaries fall.
//! Reach for the codec layer when something else already owns the track, such as a
//! `moq_mux::container::Producer` also managing a timeline and a catalog estimate.
//!
//! [`window`] is the one layer: where its groups roll follows from its own trim history, so it
//! owns the track outright.

mod diff;
pub mod snapshot;
pub mod stream;
pub mod window;

pub use crate::diff::{Diff, diff};

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

	/// A compressed frame could not be decoded (malformed, truncated, or oversized).
	#[error(transparent)]
	Flate(#[from] moq_flate::Error),

	/// A [`window`] operation is inconsistent with the window it applies to, such as a trim
	/// reaching past the oldest record.
	#[error("invalid window update: {0}")]
	Window(String),

	/// A merge patch arrived with no snapshot to apply it to.
	///
	/// Every group opens with a full snapshot, so this means frames reached
	/// [`snapshot::Decoder`] out of order, or a group's first frame was routed as a delta.
	#[error("delta before snapshot")]
	MissingSnapshot,

	/// A compressed [`stream`] frame was encoded but never written, so the shared DEFLATE window is
	/// ahead of what the consumer holds and nothing later in this group can be decoded.
	///
	/// Unlike [`snapshot`], a stream has no keyframe to resynchronize on, so the encoder refuses to
	/// continue rather than emit frames that cannot be read. Recover by rolling a new group and
	/// calling [`stream::Encoder::reset`].
	#[error("compression desynchronized: a frame was encoded but never written")]
	Desync,
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
