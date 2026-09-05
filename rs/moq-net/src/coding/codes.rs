//! Which stream reset registry a stream's codes are written and read with.

use crate::{Error, StreamError, ietf, lite};

/// The stream reset registry of the protocol a stream belongs to.
///
/// Every [`Reader`](super::Reader) and [`Writer`](super::Writer) carries the negotiated
/// version, which is what picks the registry here. The two wires draw on the same names,
/// but they do not agree on every value: moq-lite's are fixed by
/// [`StreamError::to_code`], while moq-transport's moved across the drafts we negotiate
/// (see the `ietf::error` module). Sending a code from the wrong table is silent, because the
/// number is valid in both.
///
/// Encoding and decoding live on one trait so a peer cannot be told a code from one table
/// and read against another: implement both halves or neither.
pub trait StreamCodes {
	/// The code to reset a stream, or send STOP_SENDING, with.
	fn encode_stream_code(&self, err: &StreamError) -> u32;

	/// Read a code the peer reset a stream (or sent STOP_SENDING) with.
	fn decode_stream_code(&self, code: u32) -> StreamError;

	/// Turn a transport failure into an [`Error`], reading a stream reset with this
	/// registry.
	///
	/// A session close is not stream-scoped, so it decodes through
	/// [`SessionError`](crate::SessionError) exactly as [`Error::from_transport`] does.
	fn transport_error<E: web_transport_trait::Error>(&self, err: E) -> Error {
		if let Some((code, _reason)) = err.session_error() {
			return crate::SessionError::from_code(code).into();
		}

		if let Some(code) = err.stream_error() {
			return self.decode_stream_code(code).into();
		}

		Error::Transport(err.to_string())
	}
}

/// moq-lite's registry, specified by draft-lcurley-moq-lite (Error Codes) and identical
/// across the versions we negotiate.
impl StreamCodes for lite::Version {
	fn encode_stream_code(&self, err: &StreamError) -> u32 {
		err.to_code()
	}

	fn decode_stream_code(&self, code: u32) -> StreamError {
		StreamError::from_code(code)
	}
}

/// moq-transport's registry, which is per draft.
impl StreamCodes for ietf::Version {
	fn encode_stream_code(&self, err: &StreamError) -> u32 {
		ietf::error::to_stream_code(err, *self)
	}

	fn decode_stream_code(&self, code: u32) -> StreamError {
		ietf::error::from_stream_code(code, *self)
	}
}

/// The negotiated version before it is narrowed to one protocol, e.g. the SETUP stream a
/// pre-lite-05 session opens.
impl StreamCodes for crate::Version {
	fn encode_stream_code(&self, err: &StreamError) -> u32 {
		match self {
			Self::Lite(version) => version.encode_stream_code(err),
			Self::Ietf(version) => version.encode_stream_code(err),
		}
	}

	fn decode_stream_code(&self, code: u32) -> StreamError {
		match self {
			Self::Lite(version) => version.decode_stream_code(code),
			Self::Ietf(version) => version.decode_stream_code(code),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The registry follows the negotiated protocol, not the other way around: a group
	/// dropped for being old is a moq-lite placeholder and an INTERNAL_ERROR on the IETF
	/// wire, and the same number read back on each wire has to mean what that wire said.
	#[test]
	fn the_version_picks_the_registry() {
		let lite = crate::Version::Lite(lite::Version::Lite05);
		let ietf = crate::Version::Ietf(ietf::Version::Draft20);

		assert_eq!(lite.encode_stream_code(&StreamError::Old), StreamError::Old.to_code());
		assert_eq!(ietf.encode_stream_code(&StreamError::Old), ietf::error::INTERNAL_ERROR);

		// Both agree on a cancellation, which is the one code that has to be right.
		assert_eq!(lite.encode_stream_code(&StreamError::Cancel), ietf::error::CANCELLED);
		assert_eq!(ietf.encode_stream_code(&StreamError::Cancel), ietf::error::CANCELLED);

		// GOING_AWAY is moq-lite's 0x4 and draft-20's, but draft-17 gives 0x4 to
		// UNKNOWN_OBJECT_STATUS, so it is not sent there and not read back there.
		let draft17 = crate::Version::Ietf(ietf::Version::Draft17);
		assert_eq!(lite.decode_stream_code(0x4), StreamError::GoingAway);
		assert_eq!(ietf.decode_stream_code(0x4), StreamError::GoingAway);
		assert_eq!(draft17.decode_stream_code(0x4), StreamError::Unknown(0x4));
		assert_eq!(
			draft17.encode_stream_code(&StreamError::GoingAway),
			ietf::error::INTERNAL_ERROR
		);
	}

	/// A dropped [`Writer`](super::Writer) resets with a cancellation, and `Drop` cannot
	/// carry the bound that would reach the negotiated registry. It does not need one only
	/// while every registry we speak agrees on the value, so pin that.
	#[test]
	fn both_registries_agree_about_a_cancellation() {
		// Every negotiable version, plus the work-in-progress one `Versions::all` holds back.
		let versions = crate::Versions::all()
			.iter()
			.copied()
			.chain([crate::Version::Lite(lite::Version::Lite06Wip)])
			.collect::<Vec<_>>();

		for version in versions {
			assert_eq!(
				version.encode_stream_code(&StreamError::Cancel),
				StreamError::Cancel.to_code(),
				"{version:?} cancels with a different code than the Writer's Drop sends"
			);
		}
	}

	/// A session close is not stream-scoped, so it keeps the session registry whichever
	/// wire the stream is on.
	#[test]
	fn a_session_close_keeps_the_session_registry() {
		#[derive(Debug, thiserror::Error)]
		#[error("failed")]
		struct Failed {
			session: Option<u32>,
			stream: Option<u32>,
		}

		impl web_transport_trait::Error for Failed {
			fn session_error(&self) -> Option<(u32, String)> {
				self.session.map(|code| (code, "closed".to_string()))
			}
			fn stream_error(&self) -> Option<u32> {
				self.stream
			}
		}

		let version = ietf::Version::Draft20;

		// 0x0 ends a session cleanly, but fails a stream.
		assert!(matches!(
			version.transport_error(Failed {
				session: Some(0x0),
				stream: None
			}),
			Error::Cancel
		));
		assert!(matches!(
			version.transport_error(Failed {
				session: None,
				stream: Some(0x0)
			}),
			Error::Remote(0)
		));

		// Neither: the transport itself failed.
		assert!(matches!(
			version.transport_error(Failed {
				session: None,
				stream: None
			}),
			Error::Transport(_)
		));
	}
}
