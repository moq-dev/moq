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

	/// A range object's table does not match its filename bounds.
	#[error("table bounds {smallest}..={largest} do not match the key")]
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

	/// A create collided with different object bytes.
	#[error("conflicting object: {0}")]
	Conflict(String),

	/// `.info` JSON is malformed.
	#[error("json: {0}")]
	Json(String),
}

impl From<object_store::Error> for Error {
	fn from(err: object_store::Error) -> Self {
		match err {
			object_store::Error::NotFound { path, .. } => Self::NotFound(path),
			other => Self::Store(other.to_string()),
		}
	}
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Self::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
