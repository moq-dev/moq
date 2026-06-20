//! Errors for the SRT ingest gateway.

use std::sync::Arc;

/// Errors produced while ingesting SRT into MoQ.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport (e.g. publishing into the origin).
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the moq-mux muxer/demuxer (TS demux on ingest, TS mux on egress).
	#[error("mux: {0}")]
	Mux(#[from] moq_mux::Error),

	/// I/O error from the SRT listener or socket.
	#[error("io: {0}")]
	Io(Arc<std::io::Error>),

	/// Catch-all for ingest logic that reports via `anyhow` (the moq-mux
	/// demuxer surfaces its errors this way).
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

/// Result alias for the SRT ingest gateway.
pub type Result<T> = std::result::Result<T, Error>;
