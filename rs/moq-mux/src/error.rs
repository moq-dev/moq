/// Errors from moq-mux operations.
///
/// Most variants are delegations to underlying layers — [`moq_net::Error`] for
/// transport / pub-sub failures, [`hang::Error`] for catalog/codec parsing, the
/// per-format Errors for container shape problems, and the per-codec Errors for
/// bitstream parsing problems.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the hang catalog/codec layer.
	#[error("hang: {0}")]
	Hang(#[from] hang::Error),

	/// Error parsing or building CMAF moof+mdat fragments.
	#[error("cmaf: {0}")]
	Cmaf(#[from] crate::container::fmp4::Error),

	/// Error parsing or building MKV / WebM streams.
	#[error("mkv: {0}")]
	Mkv(#[from] crate::container::mkv::Error),

	/// Error during HLS ingest.
	#[error("hls: {0}")]
	Hls(#[from] crate::container::hls::Error),

	/// Error decoding the MSF catalog.
	#[error("msf: {0}")]
	Msf(#[from] crate::catalog::msf::Error),

	/// Error parsing or building LOC frames.
	#[error("loc: {0}")]
	Loc(#[from] moq_loc::Error),

	/// Error parsing an Annex B NAL stream.
	#[error("annexb: {0}")]
	Annexb(#[from] crate::codec::annexb::Error),

	/// Error parsing AAC.
	#[error("aac: {0}")]
	Aac(#[from] crate::codec::aac::Error),

	/// Error parsing Opus.
	#[error("opus: {0}")]
	Opus(#[from] crate::codec::opus::Error),

	/// Error parsing H.264.
	#[error("h264: {0}")]
	H264(#[from] crate::codec::h264::Error),

	/// Error parsing H.265.
	#[error("h265: {0}")]
	H265(#[from] crate::codec::h265::Error),

	/// Error parsing AV1.
	#[error("av1: {0}")]
	Av1(#[from] crate::codec::av1::Error),

	/// Timestamp overflow when converting between timescales.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_net::TimeOverflow),

	/// Error decoding or encoding an mp4 atom.
	#[error("mp4: {0}")]
	Mp4(std::sync::Arc<mp4_atom::Error>),

	/// I/O error.
	#[error("io: {0}")]
	Io(std::sync::Arc<std::io::Error>),

	/// URL parse error.
	#[error("url: {0}")]
	Url(#[from] url::ParseError),

	/// Unknown media format.
	#[error("unknown format: {0}")]
	UnknownFormat(String),

	/// Buffer was not fully consumed.
	#[error("buffer was not fully consumed")]
	BufferNotConsumed,

	/// Importer dispatcher cannot return a single track for multi-track containers.
	#[error("{0} can contain multiple tracks")]
	MultipleTracks(&'static str),
}

impl From<mp4_atom::Error> for Error {
	fn from(err: mp4_atom::Error) -> Self {
		Error::Mp4(std::sync::Arc::new(err))
	}
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::Io(std::sync::Arc::new(err))
	}
}

/// A Result type alias for moq-mux operations.
pub type Result<T> = std::result::Result<T, Error>;
