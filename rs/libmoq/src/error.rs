use std::sync::Arc;

use crate::ffi;

/// Status code returned by FFI functions.
///
/// Negative values indicate errors, zero indicates success,
/// and positive values are valid resource handles.
pub type Status = i32;

/// Error types that can occur in the FFI layer.
///
/// Each error variant maps to a specific negative error code
/// returned to C callers.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying MoQ protocol layer.
	#[error("moq error: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the native helper layer (moq-native).
	#[error("native error: {0}")]
	Native(#[from] moq_native::Error),

	/// URL parsing error.
	#[error("url error: {0}")]
	Url(#[from] url::ParseError),

	/// UTF-8 string validation error.
	#[error("utf8 error: {0}")]
	Utf8(#[from] std::str::Utf8Error),

	/// Connection establishment error.
	#[error("connect error: {0}")]
	Connect(Arc<anyhow::Error>),

	/// Null or invalid pointer passed from C.
	#[error("invalid pointer")]
	InvalidPointer,

	/// Invalid resource ID.
	#[error("invalid id")]
	InvalidId,

	/// Resource not found.
	#[error("not found")]
	NotFound,

	/// Session task not found.
	#[error("session not found")]
	SessionNotFound,

	/// Origin producer not found.
	#[error("origin not found")]
	OriginNotFound,

	/// Announcement not found.
	#[error("announcement not found")]
	AnnouncementNotFound,

	/// Broadcast not found.
	#[error("broadcast not found")]
	BroadcastNotFound,

	/// Catalog not found.
	#[error("catalog not found")]
	CatalogNotFound,

	/// Media decoder not found.
	#[error("media not found")]
	MediaNotFound,

	/// Track task not found.
	#[error("track not found")]
	TrackNotFound,

	/// Group producer not found.
	#[error("group not found")]
	GroupNotFound,

	/// Frame not found.
	#[error("frame not found")]
	FrameNotFound,

	/// Unknown media format specified.
	#[error("unknown format: {0}")]
	UnknownFormat(String),

	/// Media decoder initialization failed.
	#[error("init failed: {0}")]
	InitFailed(Arc<anyhow::Error>),

	/// Media frame decode failed.
	#[error("decode failed: {0}")]
	DecodeFailed(Arc<anyhow::Error>),

	/// Timestamp value overflow.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_net::TimeOverflow),

	/// Log level parsing error.
	#[error("level error: {0}")]
	Level(Arc<tracing::metadata::ParseLevelError>),

	/// Invalid error code conversion.
	#[error("invalid code")]
	InvalidCode,

	/// Panic occurred in Rust code.
	#[error("panic")]
	Panic,

	/// Session is offline.
	#[error("offline")]
	Offline,

	/// Connection was rejected as unauthorized by the server.
	#[error("unauthorized")]
	Unauthorized,

	/// Connection was forbidden by the server.
	#[error("forbidden")]
	Forbidden,

	/// Error from the hang media layer.
	#[error("hang error: {0}")]
	Hang(#[from] hang::Error),

	/// Error from the moq-mux consumer layer.
	#[error("mux error: {0}")]
	Mux(Arc<moq_mux::Error>),

	/// Index out of bounds.
	#[error("no index")]
	NoIndex,

	/// Null byte found in C string.
	#[error("nul error")]
	NulError(#[from] std::ffi::NulError),

	/// Error from the moq-audio codec layer.
	#[error("audio error: {0}")]
	Audio(Arc<moq_audio::AudioError>),

	/// Invalid JSON passed for an application catalog section.
	#[error("json error: {0}")]
	Json(Arc<serde_json::Error>),

	/// Error from the moq-json snapshot/stream layer.
	#[error("json track error: {0}")]
	JsonTrack(Arc<moq_json::Error>),
}

impl From<moq_json::Error> for Error {
	fn from(err: moq_json::Error) -> Self {
		match err {
			moq_json::Error::Net(e) => Error::Moq(e),
			e => Error::JsonTrack(Arc::new(e)),
		}
	}
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(Arc::new(err))
	}
}

impl From<moq_audio::AudioError> for Error {
	fn from(err: moq_audio::AudioError) -> Self {
		Error::Audio(Arc::new(err))
	}
}

impl From<tracing::metadata::ParseLevelError> for Error {
	fn from(err: tracing::metadata::ParseLevelError) -> Self {
		Error::Level(Arc::new(err))
	}
}

impl From<moq_mux::Error> for Error {
	fn from(err: moq_mux::Error) -> Self {
		match err {
			moq_mux::Error::Moq(e) => Error::Moq(e),
			moq_mux::Error::Hang(e) => Error::Hang(e),
			e => Error::Mux(Arc::new(e)),
		}
	}
}

impl ffi::ReturnCode for Error {
	fn error(&self) -> Option<&Error> {
		Some(self)
	}

	fn code(&self) -> i32 {
		match self {
			Error::Moq(_) => -2,
			Error::Url(_) => -3,
			Error::Utf8(_) => -4,
			Error::Connect(_) => -5,
			Error::InvalidPointer => -6,
			Error::InvalidId => -7,
			Error::NotFound => -8,
			Error::UnknownFormat(_) => -9,
			Error::InitFailed(_) => -10,
			Error::DecodeFailed(_) => -11,
			Error::TimestampOverflow(_) => -13,
			Error::Level(_) => -14,
			Error::InvalidCode => -15,
			Error::Panic => -16,
			Error::Offline => -17,
			Error::Hang(_) => -18,
			Error::NoIndex => -19,
			Error::NulError(_) => -20,
			Error::SessionNotFound => -21,
			Error::OriginNotFound => -22,
			Error::AnnouncementNotFound => -23,
			Error::BroadcastNotFound => -24,
			Error::CatalogNotFound => -25,
			Error::MediaNotFound => -26,
			Error::TrackNotFound => -27,
			Error::FrameNotFound => -28,
			Error::Mux(_) => -29,
			Error::Audio(_) => -30,
			Error::GroupNotFound => -31,
			Error::Native(_) => -32,
			Error::Unauthorized => -33,
			Error::Forbidden => -34,
			Error::Json(_) => -35,
			Error::JsonTrack(_) => -36,
		}
	}
}
