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

	/// `Kind::Named` asked for an encoder this build does not have for that
	/// codec: a name that is not a backend, one whose feature is off, or one
	/// that does not encode the codec requested.
	#[error("no encoder named {name} for {codec} (this build has: {available})")]
	UnknownEncoder {
		/// The name that was asked for.
		name: String,
		/// The codec it was asked for.
		codec: String,
		/// The encoders this build does have for that codec.
		available: String,
	},

	/// `Kind::Named` asked for a decoder this build does not have for that
	/// codec: a name that is not a backend, one whose feature is off, or one
	/// that does not decode the codec requested.
	#[error("no decoder named {name} for {codec} (this build has: {available})")]
	UnknownDecoder {
		/// The name that was asked for.
		name: String,
		/// The codec it was asked for.
		codec: String,
		/// The decoders this build does have for that codec.
		available: String,
	},

	/// A track's codec is not supported by the native decoders.
	#[error("unsupported codec for native decode: {0}")]
	UnsupportedCodec(String),

	/// The requested capture source or enumeration has no implementation on this
	/// platform (the message names what is missing).
	#[error("not supported on this platform: {0}")]
	Unsupported(String),

	/// The operating system denied access to a capture source.
	#[error("capture permission denied: {0}")]
	PermissionDenied(String),

	/// A requested capture source does not exist or disappeared while capturing.
	#[error("capture source unavailable: {0}")]
	SourceUnavailable(String),

	/// The configured framerate is outside the supported range.
	#[error("invalid framerate: {0} (must be between 1 and 1,000,000)")]
	InvalidFramerate(u32),

	/// This encoder can't change its bitrate once open, so it can't follow a
	/// congestion-control estimate. Encoding continues at the configured rate.
	#[error("encoder {0} cannot change bitrate while running")]
	BitrateUnsupported(&'static str),

	/// GPU rendering failure: building the pipeline, importing a frame's surface
	/// as a texture, or the device itself.
	#[error("render: {0}")]
	Render(#[source] anyhow::Error),

	/// Capture / encode / codec failure (the message carries the detail).
	#[error(transparent)]
	Codec(#[from] anyhow::Error),

	/// moq-mux muxer/catalog error.
	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	/// moq-net transport error.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// Timestamp overflow converting to the moq microsecond timescale.
	#[error(transparent)]
	TimeOverflow(#[from] moq_net::TimeOverflow),
}
