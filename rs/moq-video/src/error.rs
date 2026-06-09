/// Errors returned by `moq-video`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// libav* (capture, scaling, or codec) failure.
	#[error("ffmpeg: {0}")]
	Ffmpeg(#[from] ffmpeg_next::Error),

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

	/// moq-mux codec/transport error (H.264 import, catalog).
	#[error(transparent)]
	Codec(#[from] anyhow::Error),

	/// moq-net transport error.
	#[error(transparent)]
	Moq(#[from] moq_net::Error),

	/// Timestamp overflow converting to the moq microsecond timescale.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),
}
