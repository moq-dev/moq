use std::sync::Arc;

/// Error types for the hang media library.
///
/// This enum represents all possible errors that can occur when working with
/// hang media streams, codecs, and containers.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying MoQ transport layer.
	#[error("moq lite error: {0}")]
	Moq(#[from] moq_lite::Error),

	/// JSON serialization/deserialization error.
	#[error("json error: {0}")]
	Json(Arc<serde_json::Error>),

	/// The specified codec is invalid or malformed.
	#[error("invalid codec")]
	InvalidCodec,

	/// Failed to parse an integer value.
	#[error("expected int")]
	ExpectedInt(#[from] std::num::ParseIntError),

	/// Failed to decode hexadecimal data.
	#[error("hex error: {0}")]
	Hex(#[from] hex::FromHexError),

	/// The timestamp is too large.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_lite::TimeOverflow),

	/// The track must start with a keyframe.
	#[error("must start with a keyframe")]
	MissingKeyframe,

	/// The timestamp of each keyframe must be monotonically increasing.
	#[error("timestamp went backwards")]
	TimestampBackwards,

	/// Failed to parse a URL.
	#[error("url parse error: {0}")]
	Url(#[from] url::ParseError),

	/// The format is not recognized.
	#[error("unknown format: {0}")]
	UnknownFormat(String),
}

/// A Result type alias for hang operations.
///
/// This is used throughout the hang crate as a convenient shorthand
/// for `std::result::Result<T, hang::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

// Wrap in an Arc so it is Clone
impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(Arc::new(err))
	}
}
