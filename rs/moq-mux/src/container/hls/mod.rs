//! HLS playlist ingest.
//!
//! Watches an HLS master or media playlist, downloads each fMP4 segment
//! as it appears, and feeds it through the fMP4 importer. Import-only;
//! moq-mux doesn't emit HLS today.

mod import;

pub use import::*;

/// HLS ingest errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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

	#[error("init segment was not fully consumed")]
	InitNotConsumed,

	#[error("init segment did not initialize the importer")]
	InitNotInitialized,

	#[error("encountered segment with empty URI")]
	EmptySegmentUri,

	#[error("importer not initialized for {0:?} after ensure_init_segment - init segment processing failed")]
	ImporterNotInitialized(String),

	#[error("url parse: {0}")]
	UrlParse(#[from] url::ParseError),

	#[error("reqwest: {0}")]
	Reqwest(std::sync::Arc<reqwest::Error>),

	#[error("io: {0}")]
	Io(std::sync::Arc<std::io::Error>),
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

pub type Result<T> = std::result::Result<T, Error>;
