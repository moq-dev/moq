//! moq-lite's session close boundary.

use crate::{Error, SessionError};

/// The code to close a moq-lite session with.
pub(crate) fn to_session_code(error: &SessionError) -> u32 {
	match error {
		// moq-lite's own range is assigned only by its tables; a code from elsewhere
		// must not pick up a meaning there.
		SessionError::Unknown(0x30..=0x3f) => SessionError::Internal.to_code(),
		error => error.to_code(),
	}
}

/// Classify control bytes at a stream boundary, keeping a transport FIN/reset distinct.
#[cfg(feature = "async")]
pub(super) fn control(session: &mut crate::driver::Transport, err: Error) -> Error {
	use crate::transport::poll::Session as _;
	let err = match err {
		Error::Decode(crate::DecodeError::Short) => err,
		err => crate::auth::protocol_error(err),
	};
	if matches!(err, Error::ProtocolViolation) {
		session.close(SessionError::ProtocolViolation.to_code(), &err.to_string());
	}
	err
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn an_unknown_code_does_not_gain_a_lite_meaning() {
		assert_eq!(to_session_code(&SessionError::Unsupported), 0x30);
		assert_eq!(SessionError::from_code(0x30), SessionError::Unsupported);
		assert_eq!(to_session_code(&SessionError::Expired), 0x18);
		for code in 0x30..=0x3f {
			assert_eq!(to_session_code(&SessionError::Unknown(code)), 0x1);
		}
		assert_eq!(to_session_code(&SessionError::Unknown(0x20)), 0x20);
		assert_eq!(to_session_code(&SessionError::App(7)), 64 + 7);
	}
}
