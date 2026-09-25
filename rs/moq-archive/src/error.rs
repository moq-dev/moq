/// Errors from encoding, decoding, or storing recording objects.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// The backing object store failed.
	#[error("object store: {0}")]
	Store(String),

	/// No object exists at this key.
	#[error("not found: {0}")]
	NotFound(String),

	/// The recording format version is not 1.
	#[error("unknown version {0}")]
	Version(u64),

	/// A group ID, segment ID, or frame timestamp is outside 0 through 2^53 - 1.
	#[error("{0} exceeds the recording id range")]
	Id(u64),

	/// `.info` timescale is not in 1 through 2^53 - 1.
	#[error("timescale {0} is outside 1..=9007199254740991")]
	Timescale(u64),

	/// An existing `.info` has a different priority.
	#[error("priority {existing} does not match {intended}")]
	Priority { existing: u8, intended: u8 },

	/// An existing `.info` has a different timescale.
	#[error("timescale {existing} does not match {intended}")]
	TimescaleMismatch { existing: u64, intended: u64 },

	/// The object has no groups.
	#[error("empty object")]
	Empty,

	/// Group sequences are not strictly ascending.
	#[error("group sequences are not strictly ascending")]
	Sequence,

	/// A group range is empty, reversed, or does not match an object's table.
	#[error("invalid or mismatched group bounds {smallest}..={largest}")]
	Bounds { smallest: u64, largest: u64 },

	/// The binary table is truncated, overlapping, gapped, or out of range.
	#[error("malformed table")]
	Table,

	/// Arithmetic overflowed while encoding or decoding.
	#[error("overflow")]
	Overflow,

	/// The track name is empty or its encoding is not canonical.
	#[error("invalid track name")]
	Track,

	/// An object path is not a recording key.
	#[error("invalid path: {0}")]
	Path(String),

	/// A backend returned a directory from the flat object listing API.
	#[error("directory listing is unsupported: {0}")]
	Directory(String),

	/// A pagination continuation was passed to streaming list.
	#[error("continuation query requires list_paginated")]
	Pagination,

	/// A create collided with different object bytes.
	#[error("conflicting object: {0}")]
	Conflict(String),

	/// `.info` JSON is malformed.
	#[error("json: {0}")]
	Json(String),

	/// Publishing the replayed archive failed.
	#[error("moq: {0}")]
	Moq(String),

	/// The track was already enrolled, or is the recording's own timeline.
	#[error("track already enrolled: {0}")]
	Enrolled(String),

	/// The source broadcast or one of its tracks failed.
	#[error("source: {0}")]
	Source(String),

	/// The recording's timeline could not be recovered, segmented, or published.
	#[error("timeline: {0}")]
	Timeline(String),

	/// The writer stopped accepting commands.
	#[error("writer closed")]
	Closed,
}

impl From<object_store::Error> for Error {
	fn from(err: object_store::Error) -> Self {
		match err {
			object_store::Error::NotFound { path, .. } => Self::NotFound(path),
			other => Self::Store(other.to_string()),
		}
	}
}

impl From<moq_net::Error> for Error {
	fn from(err: moq_net::Error) -> Self {
		Self::Moq(err.to_string())
	}
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Self::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
