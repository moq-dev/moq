//! Stream reset codes for the moq-transport wire.

use crate::Error;

/// The stream died on our side, with no registry entry for why.
///
/// Deliberately separate from [`Error::to_code`], which encodes moq-lite's space. Those
/// codes are our own and unstandardized; these are the registry every moq-transport peer
/// reads. The same number means different things on the two wires, so the mappings must
/// not be interchanged: moq-lite's cancel is 0, which here is this.
pub const INTERNAL_ERROR: u32 = 0x0;

/// The stream was cancelled by either endpoint.
pub const CANCELLED: u32 = 0x1;

// Draft-19 section 3.3.4 also defines DELIVERY_TIMEOUT (0x2) and SESSION_CLOSED (0x3).
// Neither is named here because nothing we can currently detect earns them: the first is
// the negotiated timeout of section 8 rather than any expiry, and the second asserts the
// whole session is going away. Add them alongside the path that can actually prove one.

/// The code to reset a stream or send STOP_SENDING with.
///
/// Only cancellation maps, because only cancellation has a meaning both spaces agree on.
/// The rest of the registry is narrower than it looks: DELIVERY_TIMEOUT is the negotiated
/// timeout of draft-19 section 8, not any expiry we happen to hit, and SESSION_CLOSED
/// asserts the session is going away rather than one handle or one piece of content. Our
/// generic errors do not establish either, so claiming them would tell a peer something
/// specific and untrue.
///
/// Everything else is INTERNAL_ERROR, which is the honest answer: the stream died on our
/// side and we have no registry entry for why.
pub fn to_stream_code(err: &Error) -> u32 {
	match err {
		Error::Cancel => CANCELLED,
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
	/// meaning a peer would read as something specific. That includes errors whose names
	/// echo a registry entry: our timeouts are not the negotiated delivery timeout, and a
	/// closed handle is not a closing session.
	#[test]
	fn unmapped_errors_are_internal() {
		for err in [
			Error::NotFound,
			Error::Duplicate,
			Error::Timeout,
			Error::Closed,
			Error::Dropped,
		] {
			assert_eq!(to_stream_code(&err), INTERNAL_ERROR, "{err:?} has no registry meaning");
		}
	}
}
