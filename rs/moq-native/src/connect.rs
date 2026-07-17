/// Error returned when connection setup fails for a terminal auth reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
	/// The server rejected the credentials (HTTP 401). Retrying with the same
	/// token will fail again.
	#[error("unauthorized")]
	Unauthorized,

	/// The credentials were understood but don't grant access to this path
	/// (HTTP 403).
	#[error("forbidden")]
	Forbidden,
}

impl ConnectError {
	pub(crate) fn from_status_u16(status: u16) -> Option<Self> {
		match status {
			401 => Some(Self::Unauthorized),
			403 => Some(Self::Forbidden),
			_ => None,
		}
	}

	/// Whether this is an authentication failure, meaning a retry is pointless
	/// until the credentials change.
	pub fn is_auth(&self) -> bool {
		matches!(self, Self::Unauthorized | Self::Forbidden)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn auth_statuses_are_terminal() {
		assert_eq!(ConnectError::from_status_u16(401), Some(ConnectError::Unauthorized));
		assert_eq!(ConnectError::from_status_u16(403), Some(ConnectError::Forbidden));
	}

	#[test]
	fn non_auth_statuses_are_not_terminal() {
		for status in [400, 404, 500] {
			assert_eq!(ConnectError::from_status_u16(status), None);
		}
	}
}
