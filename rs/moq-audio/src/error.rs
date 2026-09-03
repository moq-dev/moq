/// Errors returned by `moq-audio`.
///
/// `Clone` so a failure can be reported to more than one observer, matching
/// `hang::Error`, `moq_mux::Error`, and `moq_net::Error`.
#[derive(Clone, Debug, thiserror::Error)]
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

	/// The playback backend failed: the output device could not be opened, or the
	/// stream stopped and could not be restarted. The device or host API is at
	/// fault, not the configuration; `playback::devices` reports what is
	/// available.
	#[error("audio playback: {0}")]
	Playback(String),

	/// A packet could not be decoded: truncated, corrupt, or using a codec
	/// feature this build doesn't implement. The stream itself may be fine, so a
	/// consumer can log this one and read the next packet.
	#[error("audio decode: {0}")]
	Decode(String),

	/// The input buffer was not aligned to the codec's frame size.
	#[error("input buffer length {got} bytes does not match expected {expected}")]
	Misaligned {
		/// The buffer length received, in bytes.
		got: usize,
		/// The buffer length required, in bytes.
		expected: usize,
	},

	/// The sample-rate converter could not be constructed.
	#[error("resample construction: {0}")]
	ResamplerConstruction(String),

	/// The sample-rate converter rejected an input buffer or ratio change.
	#[error("resample: {0}")]
	Resample(String),

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
