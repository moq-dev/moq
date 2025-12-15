use std::sync::Arc;

/// Error types for the hang media library.
///
/// This enum represents all possible errors that can occur when working with
/// hang media streams, codecs, and containers.
#[derive(Debug, thiserror::Error, Clone)]
pub enum Error {
	/// An error from the underlying MoQ transport layer.
	#[error("moq lite error: {0}")]
	Moq(#[from] moq_lite::Error),

	/// Failed to decode a message at the MoQ transport layer.
	#[error("decode error: {0}")]
	Decode(#[from] moq_lite::coding::DecodeError),

	/// JSON serialization/deserialization error.
	#[error("json error: {0}")]
	Json(Arc<serde_json::Error>),

	/// Attempted to add a track that already exists in the catalog.
	#[error("duplicate track")]
	DuplicateTrack,

	/// Referenced track was not found in the catalog.
	#[error("missing track")]
	MissingTrack,

	/// The provided session ID is invalid or malformed.
	#[error("invalid session ID")]
	InvalidSession,

	/// Attempted to process an empty group (no frames).
	#[error("empty group")]
	EmptyGroup,

	/// The specified codec is invalid or malformed.
	#[error("invalid codec")]
	InvalidCodec,

	/// The frame data is invalid or corrupted.
	#[error("invalid frame")]
	InvalidFrame,

	/// The codec is not supported by this implementation.
	#[error("unsupported codec")]
	UnsupportedCodec,

	/// Failed to parse an integer value.
	#[error("expected int")]
	ExpectedInt(#[from] std::num::ParseIntError),

	/// Failed to decode hexadecimal data.
	#[error("hex error: {0}")]
	Hex(#[from] hex::FromHexError),

	/// The timestamp is too large.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] crate::TimestampOverflow),

	/// The track must start with a keyframe.
	#[error("must start with a keyframe")]
	MissingKeyframe,

	/// The timestamp of each keyframe must be monotonically increasing.
	#[error("timestamp went backwards")]
	TimestampBackwards,

	/// An error from the HTTP client.
	#[error("http error: {0}")]
	Http(Arc<reqwest::Error>),

	/// Failed to parse a URL.
	#[error("url parse error: {0}")]
	Url(#[from] url::ParseError),
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

impl From<reqwest::Error> for Error {
	fn from(err: reqwest::Error) -> Self {
		Error::Http(Arc::new(err))
	}
}
