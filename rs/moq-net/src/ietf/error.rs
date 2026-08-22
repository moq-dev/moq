//! Stream reset codes for the moq-transport wire.

use crate::Error;

/// The stream reset codes from draft-19 section 3.3.4.
///
/// Deliberately separate from [`Error::to_code`], which encodes moq-lite's space. Those
/// codes are our own and unstandardized; these are the registry every moq-transport peer
/// reads. The same number means different things on the two wires, so the mappings must
/// not be interchanged: moq-lite's cancel is 0, which here is an internal failure.
pub const INTERNAL_ERROR: u32 = 0x0;
/// The stream was cancelled by either endpoint.
pub const CANCELLED: u32 = 0x1;
/// A delivery timeout was exceeded for this stream.
pub const DELIVERY_TIMEOUT: u32 = 0x2;
/// The session is being closed.
pub const SESSION_CLOSED: u32 = 0x3;

/// The code to reset a stream or send STOP_SENDING with.
///
/// Anything without a registry entry is an internal failure, which is what the code means:
/// the peer learns that the stream died on our side rather than being told a reason we made
/// up. A routine cancellation is the one worth distinguishing, since reporting it as a
/// failure is what distorts a publisher's error handling and telemetry.
pub fn to_stream_code(err: &Error) -> u32 {
	match err {
		Error::Cancel => CANCELLED,
		Error::Timeout => DELIVERY_TIMEOUT,
		Error::Closed | Error::Dropped => SESSION_CLOSED,
		_ => INTERNAL_ERROR,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The two spaces disagree on every value that matters, which is the whole reason this
	/// mapping exists rather than reusing `Error::to_code`.
	#[test]
	fn the_two_error_spaces_disagree() {
		assert_eq!(to_stream_code(&Error::Cancel), CANCELLED);
		assert_ne!(to_stream_code(&Error::Cancel), Error::Cancel.to_code());
		assert_eq!(
			Error::Cancel.to_code(),
			INTERNAL_ERROR,
			"moq-lite's cancel is this wire's failure"
		);
	}

	/// An error with no registry entry says "this died on our side" rather than inventing a
	/// meaning a peer would read as something specific.
	#[test]
	fn unmapped_errors_are_internal() {
		assert_eq!(to_stream_code(&Error::NotFound), INTERNAL_ERROR);
		assert_eq!(to_stream_code(&Error::Duplicate), INTERNAL_ERROR);
	}
}
