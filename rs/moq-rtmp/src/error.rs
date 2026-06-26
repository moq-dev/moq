//! Errors for the RTMP ingest gateway.

use std::sync::Arc;

/// Errors produced while ingesting RTMP into MoQ.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport (e.g. publishing into the origin).
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// I/O error from the RTMP listener or a connection.
	#[error("io: {0}")]
	Io(Arc<std::io::Error>),

	/// Catch-all for ingest logic that reports via `anyhow` (the RTMP session and
	/// the moq-mux demuxer surface their errors this way).
	#[error("{0}")]
	Other(Arc<anyhow::Error>),
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::Io(Arc::new(err))
	}
}

impl From<anyhow::Error> for Error {
	fn from(err: anyhow::Error) -> Self {
		Error::Other(Arc::new(err))
	}
}

/// Result alias for the RTMP ingest gateway.
pub type Result<T> = std::result::Result<T, Error>;
