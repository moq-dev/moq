//! Errors for the RTMP ingest gateway.

/// Errors produced while ingesting RTMP into MoQ.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport (e.g. publishing into the origin).
	#[error(transparent)]
	Moq(#[from] moq_net::Error),

	/// I/O error from the RTMP listener or a connection.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// Catch-all for ingest logic that reports via `anyhow` (the RTMP session and
	/// the moq-mux demuxer surface their errors this way).
	#[error(transparent)]
	Other(#[from] anyhow::Error),
}

/// Result alias for the RTMP ingest gateway.
pub type Result<T> = std::result::Result<T, Error>;
