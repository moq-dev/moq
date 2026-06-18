//! Errors for the HLS / LL-HLS gateway.

/// Errors produced by the HLS <-> MoQ gateway (import and export).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the moq-mux CMAF import/export layer.
	#[error("mux: {0}")]
	Mux(#[from] moq_mux::Error),

	#[error("invalid playlist URL")]
	InvalidPlaylistUrl,

	#[error("invalid file path")]
	InvalidFilePath,

	#[error("invalid file URL")]
	InvalidFileUrl,

	#[error("failed to parse media playlist: {0}")]
	ParsePlaylist(String),

	#[error("no usable variants found in master playlist")]
	NoVariants,

	#[error("playlist missing EXT-X-MAP")]
	MissingMap,

	#[error("encountered segment with empty URI")]
	EmptySegmentUri,

	#[error("url parse: {0}")]
	UrlParse(#[from] url::ParseError),

	#[error("reqwest: {0}")]
	Reqwest(std::sync::Arc<reqwest::Error>),

	#[error("io: {0}")]
	Io(std::sync::Arc<std::io::Error>),

	/// Catch-all for gateway logic that reports via `anyhow`.
	#[error("{0}")]
	Other(std::sync::Arc<anyhow::Error>),
}

impl From<reqwest::Error> for Error {
	fn from(err: reqwest::Error) -> Self {
		Error::Reqwest(std::sync::Arc::new(err))
	}
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::Io(std::sync::Arc::new(err))
	}
}

impl From<anyhow::Error> for Error {
	fn from(err: anyhow::Error) -> Self {
		Error::Other(std::sync::Arc::new(err))
	}
}

pub type Result<T> = std::result::Result<T, Error>;
