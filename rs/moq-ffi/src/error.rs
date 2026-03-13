/// Error returned by all UniFFI-exported functions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum MoqError {
	#[error(transparent)]
	Protocol(#[from] moq_lite::Error),

	#[error(transparent)]
	Media(#[from] hang::Error),

	#[error(transparent)]
	Url(#[from] url::ParseError),

	#[error(transparent)]
	TimeOverflow(#[from] moq_lite::TimeOverflow),

	#[error(transparent)]
	LogLevel(#[from] tracing::metadata::ParseLevelError),

	#[error(transparent)]
	Task(#[from] tokio::task::JoinError),

	#[error("cancelled")]
	Cancelled,

	#[error("closed")]
	Closed,

	#[error("connect: {0}")]
	Connect(String),

	#[error("codec: {0}")]
	Codec(String),

	#[error("unauthorized")]
	Unauthorized,

	#[error("log: {0}")]
	Log(String),
}
