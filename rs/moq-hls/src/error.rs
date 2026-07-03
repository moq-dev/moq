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

	/// The playlist argument looked like an HTTP(S) URL but failed to parse.
	#[error("invalid playlist URL")]
	InvalidPlaylistUrl,

	/// The playlist argument was a local path that could not be made into a `file://` URL.
	#[error("invalid file path")]
	InvalidFilePath,

	/// A `file://` URL could not be turned back into a filesystem path.
	#[error("invalid file URL")]
	InvalidFileUrl,

	/// The fetched media playlist could not be parsed.
	#[error("failed to parse media playlist: {0}")]
	ParsePlaylist(String),

	/// The master playlist contained no variant this gateway can import.
	#[error("no usable variants found in master playlist")]
	NoVariants,

	/// A media playlist had no `EXT-X-MAP`, so there is no CMAF init segment.
	#[error("playlist missing EXT-X-MAP")]
	MissingMap,

	/// A media segment had an empty URI.
	#[error("encountered segment with empty URI")]
	EmptySegmentUri,

	/// An HLS media or discontinuity sequence was too large to pack into a MoQ group sequence.
	#[error("HLS {kind} sequence {value} is too large to encode")]
	SequenceOverflow {
		/// Which sequence overflowed: `"media"` or `"discontinuity"`.
		kind: &'static str,
		/// The offending sequence value.
		value: u64,
	},

	/// A playlist or segment URI could not be resolved against its base.
	#[error("url parse: {0}")]
	UrlParse(#[from] url::ParseError),

	/// HTTP error while fetching a playlist or segment.
	#[error("reqwest: {0}")]
	Reqwest(std::sync::Arc<reqwest::Error>),

	/// I/O error while reading a local playlist or segment.
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

/// Convenience alias for results from the HLS gateway.
pub type Result<T> = std::result::Result<T, Error>;
