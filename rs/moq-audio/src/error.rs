/// Errors returned by `moq-audio`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AudioError {
	/// The codec does not support this sample rate / channel combination.
	#[error("unsupported audio configuration: {0}")]
	Unsupported(String),

	/// The input buffer was not aligned to the codec's frame size.
	#[error("input buffer length {got} bytes does not match expected {expected}")]
	Misaligned { got: usize, expected: usize },

	/// Rubato resampler construction error.
	#[error("resample construction: {0}")]
	ResamplerConstruction(#[from] rubato::ResamplerConstructionError),

	/// Rubato resampler runtime error.
	#[error("resample: {0}")]
	Resample(#[from] rubato::ResampleError),

	/// hang catalog error.
	#[error(transparent)]
	Hang(#[from] hang::Error),

	/// moq-mux container/transport error.
	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	/// moq-net transport error.
	#[error(transparent)]
	Moq(#[from] moq_net::Error),

	/// Timestamp overflow.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),
}
