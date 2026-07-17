/// Errors returned by `moq-audio`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The requested configuration is outside what the codec supports, e.g. a
	/// sample rate, channel count, or frame duration Opus can't encode. The
	/// caller asked for something impossible; picking different settings fixes it.
	#[error("unsupported audio configuration: {0}")]
	Unsupported(String),

	/// No audio device matched the requested selector, or the machine has no
	/// default input. Retrying won't help until the device list changes;
	/// `capture::devices` reports what is available.
	#[error("audio device: {0}")]
	Device(String),

	/// The capture backend failed or delivered nothing: a denied permission, a
	/// device that stopped mid-stream, or a host API error. The configuration may
	/// be fine; the device or its permissions are not.
	#[error("audio capture: {0}")]
	Capture(String),

	/// The input buffer was not aligned to the codec's frame size.
	#[error("input buffer length {got} bytes does not match expected {expected}")]
	Misaligned {
		/// The buffer length received, in bytes.
		got: usize,
		/// The buffer length required, in bytes.
		expected: usize,
	},

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
	Net(#[from] moq_net::Error),

	/// Timestamp overflow.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),
}
