/// Error types for the hang media library.
///
/// This enum represents all possible errors that can occur when working with
/// hang media streams, codecs, and containers.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying MoQ transport layer.
	#[error("moq error: {0}")]
	Moq(#[from] moq_net::Error),

	/// JSON serialization/deserialization error.
	#[error("json error: {0}")]
	Json(String),

	/// A catalog has more media renditions than a consumer will hold.
	#[error("catalog has {count} renditions, over the limit of {max}")]
	TooManyRenditions { count: usize, max: usize },

	/// A catalog broadcast reference escapes its root.
	#[error("catalog broadcast reference escapes the root: {0}")]
	EscapingBroadcast(String),

	/// The specified codec is invalid or malformed.
	#[error("invalid codec")]
	InvalidCodec,

	/// Failed to parse an integer value.
	#[error("expected int")]
	ExpectedInt(#[from] std::num::ParseIntError),

	/// Failed to decode hexadecimal data.
	#[error("hex error: {0}")]
	Hex(String),

	/// The timestamp is too large.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_net::TimeOverflow),

	/// The track must start with a keyframe.
	#[error("must start with a keyframe")]
	MissingKeyframe,

	/// The timestamp of each keyframe must be monotonically increasing.
	#[error("timestamp went backwards")]
	TimestampBackwards,

	/// Failed to parse a URL.
	#[error("url parse error: {0}")]
	Url(String),

	/// A group contained zero frames.
	#[error("empty group")]
	EmptyGroup,

	/// The format is not recognized.
	#[error("unknown format: {0}")]
	UnknownFormat(String),

	/// A track with this name already exists in the catalog.
	#[error("duplicate track: {0}")]
	Duplicate(String),

	/// A catalog timescale is zero, so no timestamp can be expressed in it.
	#[error("invalid timescale: {0}")]
	InvalidTimescale(u64),

	/// A catalog wall-clock value is outside the JSON-safe integer range.
	#[error("invalid wall clock: {0}")]
	InvalidWall(u64),

	/// The shared video rotation is not a finite number.
	#[error("video rotation must be finite")]
	InvalidVideoRotation,
}

/// A Result type alias for hang operations.
///
/// This is used throughout the hang crate as a convenient shorthand
/// for `std::result::Result<T, hang::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

// Foreign errors are flattened to their message so they stay out of this crate's public API.
impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

impl From<hex::FromHexError> for Error {
	fn from(err: hex::FromHexError) -> Self {
		Error::Hex(err.to_string())
	}
}

impl From<url::ParseError> for Error {
	fn from(err: url::ParseError) -> Self {
		Error::Url(err.to_string())
	}
}
