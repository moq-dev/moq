//! Error type for the transcoder.

/// Errors returned by `moq-transcode`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The source catalog has no rendition the transcoder can decode: it needs
	/// an H.264 or H.265 rendition local to the source broadcast.
	#[error("no transcodable video rendition in the source catalog")]
	NoSource,

	/// The chosen source rendition doesn't declare coded dimensions, so rungs
	/// can't be sized or gated against it.
	#[error("source rendition {0:?} is missing codedWidth/codedHeight")]
	SourceDimensions(String),

	/// moq-net transport error.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// moq-mux container/catalog error.
	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	/// hang catalog/container error.
	#[error(transparent)]
	Hang(#[from] hang::Error),

	/// Video decode/encode error.
	#[error(transparent)]
	Video(#[from] moq_video::Error),

	/// Timestamp overflow converting to the moq microsecond timescale.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),

	/// Frame scaling failure.
	#[error("scale failed: {0}")]
	Scale(String),
}
