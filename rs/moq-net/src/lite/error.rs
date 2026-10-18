//! The Lite additions to the shared session error registry.

use crate::{Error, SessionError};

pub(crate) fn to_session_code(error: &SessionError) -> u32 {
	match error {
		SessionError::Unsupported => 0x30,
		// moq-lite's own range is assigned only by its tables; a code from elsewhere
		// must not pick up a meaning there.
		SessionError::Unknown(0x30..=0x3f) => SessionError::Internal.to_code(),
		error => error.to_code(),
	}
}

pub(crate) fn from_session_code(code: u32) -> SessionError {
	match code {
		0x30 => SessionError::Unsupported,
		code => SessionError::from_code(code),
	}
}

pub(crate) fn from_transport(error: impl crate::transport::Error) -> Error {
	if let Some((code, _)) = error.session_error() {
		return from_session_code(code).into();
	}
	Error::from_transport(error)
}

/// Classify control bytes at a stream boundary, keeping a transport FIN/reset distinct.
pub(super) fn control<S: crate::transport::poll::Session>(session: &mut S, err: Error) -> Error {
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
		assert_eq!(from_session_code(0x30), SessionError::Unsupported);
		assert_eq!(to_session_code(&SessionError::Expired), 0x18);
		assert_eq!(from_session_code(0x18), SessionError::Expired);
		for code in 0x30..=0x3f {
			assert_eq!(to_session_code(&SessionError::Unknown(code)), 0x1);
		}
		assert_eq!(to_session_code(&SessionError::Unknown(0x20)), 0x20);
		assert_eq!(to_session_code(&SessionError::App(7)), 64 + 7);
	}
}
