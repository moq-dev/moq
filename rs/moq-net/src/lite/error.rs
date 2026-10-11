//! The Lite additions to the shared session error registry.

use crate::{Error, SessionError};

pub(crate) fn to_session_code(error: &SessionError) -> u32 {
	match error {
		SessionError::Unsupported => 0x30,
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
