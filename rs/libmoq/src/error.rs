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
	/// Resource was closed.
	#[error("closed")]
	Closed,

	/// Error from the underlying MoQ protocol layer.
	#[error("moq error: {0}")]
	Moq(#[from] moq_lite::Error),

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
	TimestampOverflow(#[from] moq_lite::TimeOverflow),

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

	/// Error from the hang media layer.
	#[error("hang error: {0}")]
	Hang(#[from] hang::Error),

	/// Index out of bounds.
	#[error("no index")]
	NoIndex,

	/// Null byte found in C string.
	#[error("nul error")]
	NulError(#[from] std::ffi::NulError),
}

impl From<tracing::metadata::ParseLevelError> for Error {
	fn from(err: tracing::metadata::ParseLevelError) -> Self {
		Error::Level(Arc::new(err))
	}
}

impl ffi::ReturnCode for Error {
	fn code(&self) -> i32 {
		tracing::error!("{}", self);
		match self {
			Error::Closed => -1,
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
		}
	}
}
