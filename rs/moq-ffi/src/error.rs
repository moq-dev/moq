/// Error returned by all UniFFI-exported functions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
#[non_exhaustive]
pub enum MoqError {
	#[error(transparent)]
	Protocol(#[from] moq_net::Error),

	#[error(transparent)]
	Media(#[from] hang::Error),

	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	#[error(transparent)]
	JsonTrack(#[from] moq_json::Error),

	#[error(transparent)]
	Audio(#[from] moq_audio::AudioError),

	#[error(transparent)]
	Url(#[from] url::ParseError),

	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),

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

	#[error("bind: {0}")]
	Bind(String),

	#[error("reject: {0}")]
	Reject(String),

	#[error("already responded")]
	AlreadyResponded,

	#[error("codec: {0}")]
	Codec(String),

	/// Failed to parse a JSON value, e.g. an invalid catalog section payload.
	#[error("json: {0}")]
	Json(String),

	#[error("invalid error code: {0}")]
	InvalidErrorCode(i32),

	#[error("unauthorized")]
	Unauthorized,

	#[error("forbidden")]
	Forbidden,

	#[error("log: {0}")]
	Log(String),
}
