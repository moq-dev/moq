//! Errors for the RTSP ingest.

/// Errors produced while pulling an RTSP session into a broadcast.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the moq-net transport, such as creating a track.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from a moq-mux importer.
	#[error("mux: {0}")]
	Mux(#[from] moq_mux::Error),

	/// Error from the RTSP session: connecting, negotiating, or depacketizing.
	#[error("rtsp: {0}")]
	Rtsp(String),

	/// The URL is not one an RTSP session can dial.
	#[error("invalid RTSP URL: {0}")]
	InvalidUrl(String),

	/// The server refused the credentials, or asked for some and the URL had none.
	#[error("RTSP server refused the credentials: {0}")]
	Unauthorized(String),

	/// The session offers no stream the stack can publish.
	#[error("no H.264, H.265, or AAC stream in the session (found {0})")]
	NoSupportedStream(String),
}

impl From<retina::Error> for Error {
	fn from(err: retina::Error) -> Self {
		match err.status_code() {
			Some(401) => Self::Unauthorized(err.to_string()),
			_ => Self::Rtsp(err.to_string()),
		}
	}
}

/// Result alias for the RTSP ingest.
pub type Result<T> = std::result::Result<T, Error>;
