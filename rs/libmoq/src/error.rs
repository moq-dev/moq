use std::sync::Arc;

use crate::ffi;

pub type Status = i32;

#[derive(Debug, thiserror::Error, Clone)]
pub enum Error {
	#[error("closed")]
	Closed,

	#[error("moq error: {0}")]
	Moq(#[from] moq_lite::Error),

	#[error("url error: {0}")]
	Url(#[from] url::ParseError),

	#[error("utf8 error: {0}")]
	Utf8(#[from] std::str::Utf8Error),

	#[error("connect error: {0}")]
	Connect(Arc<anyhow::Error>),

	#[error("invalid pointer")]
	InvalidPointer,

	#[error("invalid id")]
	InvalidId,

	#[error("not found")]
	NotFound,

	#[error("unknown format: {0}")]
	UnknownFormat(String),

	#[error("init failed: {0}")]
	InitFailed(Arc<anyhow::Error>),

	#[error("decode failed: {0}")]
	DecodeFailed(Arc<anyhow::Error>),

	#[error("timestamp overflow")]
	TimestampOverflow(#[from] hang::TimestampOverflow),

	#[error("level error: {0}")]
	Level(Arc<tracing::metadata::ParseLevelError>),

	#[error("invalid code")]
	InvalidCode,

	#[error("panic")]
	Panic,
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
		}
	}
}
