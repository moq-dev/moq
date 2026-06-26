/// Errors returned by `moq-video`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// libav* (capture, scaling, or codec) failure.
	///
	/// Carries the formatted message rather than the typed `ffmpeg_next::Error`
	/// on purpose: keeping ffmpeg out of the public surface means an
	/// `ffmpeg-next` major bump isn't a breaking change for consumers.
	#[error("ffmpeg: {0}")]
	Ffmpeg(String),

	/// No encoder matching the requested codec / hardware preference was
	/// compiled into the linked ffmpeg.
	#[error("no usable H.264 encoder found (tried: {0})")]
	NoEncoder(String),

	/// The requested input format (avfoundation / v4l2 / dshow) is not
	/// available in the linked libavdevice.
	#[error("capture backend {0:?} not available in this ffmpeg build")]
	NoCaptureBackend(&'static str),

	/// The opened capture device exposed no decodable video stream.
	#[error("no video stream on capture device {0:?}")]
	NoVideoStream(String),

	/// The configured framerate was zero (would divide by zero / produce a
	/// degenerate codec time base).
	#[error("invalid framerate: {0} (must be non-zero)")]
	InvalidFramerate(u32),

	/// moq-mux codec/container error (H.264 import, catalog).
	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	/// Ad-hoc encode/capture error.
	#[error(transparent)]
	Codec(#[from] anyhow::Error),

	/// moq-net transport error.
	#[error(transparent)]
	Moq(#[from] moq_net::Error),

	/// Timestamp overflow converting to the moq microsecond timescale.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),
}

// Manual (not `#[from]`) so the typed ffmpeg error stays out of the public
// variant while `?` on ffmpeg results still converts automatically.
impl From<ffmpeg_next::Error> for Error {
	fn from(err: ffmpeg_next::Error) -> Self {
		Self::Ffmpeg(err.to_string())
	}
}
