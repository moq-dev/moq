use crate::src::opus_defines::{
	OPUS_ALLOC_FAIL, OPUS_BAD_ARG, OPUS_BUFFER_TOO_SMALL, OPUS_INVALID_PACKET, OPUS_INVALID_STATE, OPUS_UNIMPLEMENTED,
};

/// Why an Opus call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// An argument was out of range, such as a sample rate Opus does not code.
	#[error("invalid argument")]
	BadArg,
	/// The output buffer was too small for the result.
	#[error("buffer too small")]
	BufferTooSmall,
	/// The codec failed internally.
	#[error("internal error")]
	Internal,
	/// The packet is corrupt or uses an unsupported configuration.
	#[error("corrupted stream")]
	InvalidPacket,
	/// The request is not implemented.
	#[error("request not implemented")]
	Unimplemented,
	/// The encoder or decoder state is invalid.
	#[error("invalid state")]
	InvalidState,
	/// An allocation failed.
	#[error("memory allocation failed")]
	AllocFail,
}

impl Error {
	/// The error a negative libopus return code names.
	pub(crate) fn from_code(code: i32) -> Self {
		match code {
			OPUS_BAD_ARG => Self::BadArg,
			OPUS_BUFFER_TOO_SMALL => Self::BufferTooSmall,
			OPUS_INVALID_PACKET => Self::InvalidPacket,
			OPUS_UNIMPLEMENTED => Self::Unimplemented,
			OPUS_INVALID_STATE => Self::InvalidState,
			OPUS_ALLOC_FAIL => Self::AllocFail,
			// OPUS_INTERNAL_ERROR, and any code libopus 1.3.1 never returns.
			_ => Self::Internal,
		}
	}

	/// `Ok` with a libopus return value that is not an error code.
	pub(crate) fn check(code: i32) -> Result<i32, Self> {
		if code < 0 {
			Err(Self::from_code(code))
		} else {
			Ok(code)
		}
	}
}
