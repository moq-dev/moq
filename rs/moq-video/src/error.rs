/// Errors returned by `moq-video`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// No encoder matching the requested codec / hardware preference could be
	/// opened (none compiled in, or none available on this machine).
	#[error("no usable video encoder found (tried: {0})")]
	NoEncoder(String),

	/// No decoder matching the requested codec / hardware preference could be
	/// opened (none compiled in, or none available on this machine).
	#[error("no usable video decoder found (tried: {0})")]
	NoDecoder(String),

	/// A track's codec is not supported by the native decoders.
	#[error("unsupported codec for native decode: {0}")]
	UnsupportedCodec(String),

	/// The configured framerate was zero (would divide by zero / produce a
	/// degenerate codec time base).
	#[error("invalid framerate: {0} (must be non-zero)")]
	InvalidFramerate(u32),

	/// This encoder can't change its bitrate once open, so it can't follow a
	/// congestion-control estimate. Encoding continues at the configured rate.
	#[error("encoder {0} cannot change bitrate while running")]
	BitrateUnsupported(&'static str),

	/// Capture / encode / codec failure (the message carries the detail).
	#[error(transparent)]
	Codec(#[from] anyhow::Error),

	/// moq-mux muxer/catalog error.
	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	/// moq-net transport error.
	#[error(transparent)]
	Moq(#[from] moq_net::Error),

	/// Timestamp overflow converting to the moq microsecond timescale.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),
}
