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

	// Native codec errors, behind the optional `audio`/`video` features.
	#[cfg(all(feature = "audio", not(target_arch = "wasm32")))]
	#[error(transparent)]
	Audio(#[from] moq_audio::Error),

	#[cfg(all(feature = "video", not(target_arch = "wasm32")))]
	#[error(transparent)]
	Video(#[from] moq_video::Error),

	#[error(transparent)]
	Url(#[from] url::ParseError),

	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),

	#[error(transparent)]
	LogLevel(#[from] tracing::metadata::ParseLevelError),

	// Only the native path spawns onto a runtime, so only it can fail to join.
	#[cfg(not(target_arch = "wasm32"))]
	#[error(transparent)]
	Task(#[from] tokio::task::JoinError),

	#[error("json: {0}")]
	Json(#[from] serde_json::Error),

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

	#[error("unauthorized")]
	Unauthorized,

	#[error("forbidden")]
	Forbidden,

	/// The requested track or group is not available.
	#[error("not found")]
	NotFound,

	/// The requested operation is not supported.
	#[error("unsupported")]
	Unsupported,

	/// A route carried an invalid hop id or too many hops.
	#[error("invalid route: {0}")]
	InvalidRoute(String),

	/// A catalog rendition named another broadcast, but this consumer came from a standalone
	/// broadcast rather than an origin, so there is nothing to resolve the reference against.
	#[error("unresolvable broadcast reference: {0}")]
	UnresolvableBroadcast(String),

	#[error("log: {0}")]
	Log(String),
}
