use std::sync::Arc;

/// Error returned by all UniFFI-exported functions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MoqError {
	#[error("{msg}")]
	Error { msg: String },
}

impl From<moq_lite::Error> for MoqError {
	fn from(err: moq_lite::Error) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

impl From<hang::Error> for MoqError {
	fn from(err: hang::Error) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

impl From<url::ParseError> for MoqError {
	fn from(err: url::ParseError) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

impl From<moq_lite::TimeOverflow> for MoqError {
	fn from(err: moq_lite::TimeOverflow) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

impl From<tracing::metadata::ParseLevelError> for MoqError {
	fn from(err: tracing::metadata::ParseLevelError) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

impl From<Arc<anyhow::Error>> for MoqError {
	fn from(err: Arc<anyhow::Error>) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

impl From<tokio::task::JoinError> for MoqError {
	fn from(err: tokio::task::JoinError) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}
