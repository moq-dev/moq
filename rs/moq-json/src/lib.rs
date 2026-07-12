//! JSON publishing over [`moq-net`](moq_net) tracks, in two modes:
//!
//! - [`snapshot`]: **lossy**. One JSON value updated over time; a consumer only gets the most
//!   recent value. Intermediate updates are collapsed and older groups are dropped.
//! - [`stream`]: **lossless**. An ordered append-log of self-contained records; every record is
//!   preserved and delivered in order, nothing is ever superseded.
//!
//! Pick [`snapshot`] when consumers care about "what is the value now" (a catalog, a status
//! document) and [`stream`] when they care about every record (an event log, a media timeline).

mod diff;
pub mod snapshot;
pub mod stream;

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
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
