/// Errors from moq-mux operations.
///
/// Most variants are delegations to underlying layers: [`moq_net::Error`] for
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

	/// Error publishing or consuming JSON over a track.
	#[error("json: {0}")]
	Json(#[from] moq_json::Error),

	/// Error parsing or building CMAF moof+mdat fragments.
	#[error("cmaf: {0}")]
	Cmaf(#[from] crate::container::fmp4::Error),

	/// Error parsing or building MKV / WebM streams.
	#[error("mkv: {0}")]
	Mkv(#[from] crate::container::mkv::Error),

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

	/// Error parsing FLAC.
	#[error("flac: {0}")]
	Flac(#[from] crate::codec::flac::Error),

	/// Error parsing MP3.
	#[error("mp3: {0}")]
	Mp3(#[from] crate::codec::mp3::Error),

	/// Error parsing H.264.
	#[error("h264: {0}")]
	H264(#[from] crate::codec::h264::Error),

	/// Error parsing H.265.
	#[error("h265: {0}")]
	H265(#[from] crate::codec::h265::Error),

	/// Error parsing AV1.
	#[error("av1: {0}")]
	Av1(#[from] crate::codec::av1::Error),

	/// Error parsing VP8.
	#[error("vp8: {0}")]
	Vp8(#[from] crate::codec::vp8::Error),

	/// Error parsing VP9.
	#[error("vp9: {0}")]
	Vp9(#[from] crate::codec::vp9::Error),

	/// Error parsing legacy audio (MP2 / AC-3 / E-AC-3).
	#[error("legacy: {0}")]
	Legacy(#[from] crate::codec::legacy::Error),

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

	/// A non-keyframe frame was received before any keyframe opened a group.
	/// A track joining mid-stream should skip frames until the first keyframe.
	#[error("{0}")]
	MissingKeyframe(#[from] crate::container::MissingKeyframe),

	/// Error from a muxer/demuxer that reports via `anyhow` (currently MPEG-TS).
	/// Boxed in an `Arc` so the enum stays `Clone` (`anyhow::Error` is not).
	#[error("{0}")]
	Other(std::sync::Arc<anyhow::Error>),

	/// Tried to set an application catalog section whose name collides with a
	/// reserved media section (`video`/`audio`).
	#[error("reserved catalog section: {0}")]
	ReservedSection(String),
}

impl From<anyhow::Error> for Error {
	fn from(err: anyhow::Error) -> Self {
		Error::Other(std::sync::Arc::new(err))
	}
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
