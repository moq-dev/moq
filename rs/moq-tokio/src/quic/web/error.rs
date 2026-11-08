use std::sync::OnceLock;

/// Why a WebTransport (or raw QUIC) session failed: its handshake, its connection, or a close.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// The QUIC connection failed underneath the session.
	#[error("connection error: {0}")]
	Connection(#[from] crate::quic::driver::connection::Error),

	/// The session closed with this application code and reason.
	#[error("closed: code={0} reason={1}")]
	Closed(u32, String),

	/// The peer tore down the session's streams before its close reason arrived.
	#[error("session gone")]
	SessionGone,

	/// A stream named a different session.
	#[error("unknown session")]
	UnknownSession,

	/// HTTP/3 closed the connection with a code of its own, one that names no WebTransport
	/// code (`H3_NO_ERROR`, say), so not a code the peer's application chose.
	#[error("http/3 closed: code={code} reason={reason:?}")]
	Http3 {
		/// The HTTP/3 error code.
		code: u64,
		/// The close reason.
		reason: String,
	},

	/// The HTTP/3 SETTINGS or CONNECT exchange failed.
	#[error("handshake failed: {0}")]
	Handshake(#[from] HandshakeError),

	/// A QUIC stream the session reads, such as a stream header, failed.
	#[error("read error: {0}")]
	Read(#[from] crate::quic::driver::recv::Error),

	/// A QUIC stream or datagram the session writes failed.
	#[error("write error: {0}")]
	Write(#[from] crate::quic::driver::send::Error),
}

/// Why the HTTP/3 SETTINGS or CONNECT exchange failed.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum HandshakeError {
	/// A handshake stream ended before its message did.
	#[error("stream ended early")]
	UnexpectedEnd,

	/// The peer's SETTINGS do not enable WebTransport.
	#[error("WebTransport is not supported")]
	Unsupported,

	/// The SETTINGS were malformed.
	#[error("settings: {0}")]
	Settings(#[from] web_transport_proto::SettingsError),

	/// The CONNECT request or response was malformed.
	#[error("connect: {0}")]
	Connect(#[from] web_transport_proto::ConnectError),

	/// A status that does not fit the exchange: a client answered with anything but 200, or a
	/// server asked to accept with a status other than 200, or to reject with one below 400.
	#[error("http status: {0}")]
	Status(http::StatusCode),

	/// The CONNECT response selected a subprotocol the request did not offer.
	#[error("subprotocol not offered: {0}")]
	Protocol(String),
}

impl From<web_transport_proto::SettingsError> for Error {
	fn from(err: web_transport_proto::SettingsError) -> Self {
		HandshakeError::from(err).into()
	}
}

impl From<web_transport_proto::ConnectError> for Error {
	fn from(err: web_transport_proto::ConnectError) -> Self {
		HandshakeError::from(err).into()
	}
}

/// The HTTP/3 code a closed session's streams are reset and stopped with.
const WT_SESSION_GONE: u64 = 0x170d7b68;

/// Why a session closed, shared by the session and its streams. The first close wins.
///
/// It also knows whether the session is raw QUIC, which decides how its close and stream
/// codes are encoded.
#[derive(Debug)]
pub(crate) struct CloseReason {
	// Raw QUIC carries the application code as is; HTTP/3 maps it into its own code space.
	raw: bool,
	reason: OnceLock<Error>,
}

impl CloseReason {
	pub(crate) fn new(raw: bool) -> Self {
		Self {
			raw,
			reason: OnceLock::new(),
		}
	}

	/// The QUIC code to reset or stop a stream with.
	pub(crate) fn encode_stream_code(&self, code: u32) -> crate::quic::driver::VarInt {
		if self.raw {
			return code.into();
		}
		crate::quic::driver::VarInt::try_from(web_transport_proto::error_to_http3(code)).unwrap()
	}

	/// Decode the peer's QUIC code for a stream reset or stop, or None if it is not one.
	pub(crate) fn decode_stream_code(&self, code: crate::quic::driver::VarInt) -> Option<u32> {
		let code = code.into_inner();
		// An older raw peer maps its codes into the HTTP/3 range too. No u32 reaches that
		// range, so the mapped form cannot be mistaken for a raw code.
		match web_transport_proto::error_from_http3(code) {
			Some(code) => Some(code),
			None if self.raw => u32::try_from(code).ok(),
			None => None,
		}
	}

	/// The session's close reason when the peer reset or stopped a stream with
	/// WT_SESSION_GONE, which tears down the streams of a closed session
	/// (draft-ietf-webtrans-http3 §4.4), or None for any other code.
	///
	/// The reset can beat the close capsule, so until it lands the code and reason are
	/// unknown: [`Error::SessionGone`], never a guessed clean close.
	pub(crate) fn session_gone(&self, code: crate::quic::driver::VarInt) -> Option<Error> {
		if self.raw || code.into_inner() != WT_SESSION_GONE {
			return None;
		}
		Some(self.reason.get().cloned().unwrap_or(Error::SessionGone))
	}

	/// Record the close reason, returning false if one was already recorded.
	pub(crate) fn set(&self, err: Error) -> bool {
		self.reason.set(err).is_ok()
	}

	/// Replace an error caused by the connection closing with the recorded reason, or
	/// decode the peer's close code when none was recorded.
	pub(crate) fn map(&self, err: Error) -> Error {
		use crate::quic::driver::{connection, recv, send};

		let conn = match &err {
			Error::Connection(conn)
			| Error::Read(recv::Error::Connection(conn))
			| Error::Write(send::Error::Connection(conn)) => Some(conn),
			Error::Closed(..) | Error::SessionGone | Error::Http3 { .. } => None,
			_ => return err,
		};

		if let Some(reason) = self.reason.get() {
			return reason.clone();
		}

		let Some(connection::Error::ApplicationClosed(close)) = conn else {
			return err;
		};
		let code = close.error_code.into_inner();
		let reason = close.reason.clone();
		if self.raw {
			return match u32::try_from(code) {
				Ok(code) => Error::Closed(code, reason),
				Err(_) => err,
			};
		}
		match web_transport_proto::error_from_http3(code) {
			Some(code) => Error::Closed(code, reason),
			None => Error::Http3 { code, reason },
		}
	}
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		use crate::quic::driver::{connection, recv, send};
		let conn = match self {
			Self::Connection(conn)
			| Self::Read(recv::Error::Connection(conn))
			| Self::Write(send::Error::Connection(conn)) => conn,
			_ => return moq_net::transport::CloseKind::Transport,
		};
		match conn {
			connection::Error::TimedOut => moq_net::transport::CloseKind::IdleTimeout,
			connection::Error::LocallyClosed { .. } => moq_net::transport::CloseKind::Closed,
			_ => moq_net::transport::CloseKind::Transport,
		}
	}
	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::Closed(code, reason) => Some((*code, reason.clone())),
			_ => None,
		}
	}
	fn stream_error(&self) -> Option<u32> {
		// An open can fail while writing its WebTransport header, before it returns a stream.
		match self {
			Self::Write(crate::quic::driver::send::Error::Stopped(code)) => {
				web_transport_proto::error_from_http3(code.into_inner())
			}
			_ => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn transport_error_kinds_preserve_stream_and_session_registries() {
		use moq_net::transport::{CloseKind, Error as _};
		let idle = Error::Connection(crate::quic::driver::connection::Error::TimedOut);
		assert_eq!(idle.close_kind(), CloseKind::IdleTimeout);
		assert_eq!(
			super::super::recv::Error::Session(idle.clone()).close_kind(),
			CloseKind::IdleTimeout
		);
		assert_eq!(
			super::super::send::Error::Session(idle).close_kind(),
			CloseKind::IdleTimeout
		);
		assert_eq!(super::super::recv::Error::Evicted.close_kind(), CloseKind::Evicted);
		let local = Error::Connection(crate::quic::driver::connection::Error::LocallyClosed {
			code: 7u32.into(),
			reason: String::new(),
		});
		assert_eq!(local.close_kind(), CloseKind::Closed);
		let session = Error::Closed(7, "session".into());
		assert_eq!(session.session_error(), Some((7, "session".into())));
		assert_eq!(session.stream_error(), None);
		let stream = super::super::recv::Error::Reset(7);
		assert_eq!(stream.session_error(), None);
		assert_eq!(stream.stream_error(), Some(7));
		let close = CloseReason::new(false);
		let header = Error::Write(crate::quic::driver::send::Error::Stopped(close.encode_stream_code(7)));
		assert_eq!(header.stream_error(), Some(7));
		let invalid = Error::Write(crate::quic::driver::send::Error::Stopped(7u32.into()));
		assert_eq!(invalid.stream_error(), None);
	}

	/// WebTransport keeps the HTTP/3 mapping in both directions, and a code outside its
	/// range is not a WebTransport code.
	#[test]
	fn http3_stream_codes_stay_mapped() {
		let close = CloseReason::new(false);
		let mapped = close.encode_stream_code(5);
		assert_eq!(mapped.into_inner(), web_transport_proto::error_to_http3(5));
		assert_eq!(close.decode_stream_code(mapped), Some(5));
		assert_eq!(close.decode_stream_code(5u32.into()), None);
	}

	/// Raw QUIC sends the code as is and reads both the raw and the legacy mapped form.
	#[test]
	fn raw_stream_codes_pass_through() {
		let close = CloseReason::new(true);
		assert_eq!(close.encode_stream_code(5).into_inner(), 5);
		assert_eq!(close.decode_stream_code(5u32.into()), Some(5));
		assert_eq!(close.decode_stream_code(u32::MAX.into()), Some(u32::MAX));

		let legacy = crate::quic::driver::VarInt::try_from(web_transport_proto::error_to_http3(5)).unwrap();
		assert_eq!(close.decode_stream_code(legacy), Some(5));

		// Past u32 and outside the HTTP/3 range: neither form.
		let invalid = crate::quic::driver::VarInt::from_u64(1 << 40).unwrap();
		assert_eq!(close.decode_stream_code(invalid), None);
	}

	/// WT_SESSION_GONE reports the session's close, before and after its capsule arrives.
	#[test]
	fn session_gone_reads_as_the_close() {
		let gone = crate::quic::driver::VarInt::from_u64(WT_SESSION_GONE).unwrap();
		let close = CloseReason::new(false);
		assert!(matches!(close.session_gone(gone), Some(Error::SessionGone)));

		close.set(Error::Closed(7, "bye".into()));
		assert!(matches!(
			close.session_gone(gone),
			Some(Error::Closed(7, reason)) if reason == "bye"
		));
		assert!(close.session_gone(close.encode_stream_code(7)).is_none());

		// Raw QUIC has no HTTP/3 code space, so the same number is an application code.
		assert!(CloseReason::new(true).session_gone(gone).is_none());
	}

	/// A peer close maps into WebTransport code space, or is HTTP/3's own failure when its
	/// code names none, and raw QUIC passes the code through.
	#[test]
	fn a_peer_close_decodes_by_mode() {
		use moq_net::transport::Error as _;
		let closed = |code: u64| {
			Error::Connection(crate::quic::driver::connection::Error::ApplicationClosed(
				moq_quic::ApplicationClose {
					error_code: crate::quic::driver::VarInt::from_u64(code).unwrap(),
					reason: "bye".into(),
				},
			))
		};

		let web = CloseReason::new(false);
		let mapped = web.map(closed(web_transport_proto::error_to_http3(7)));
		assert_eq!(mapped.session_error(), Some((7, "bye".into())));
		let h3 = web.map(closed(0x100));
		assert!(
			matches!(&h3, Error::Http3 { code: 0x100, reason } if reason == "bye"),
			"got {h3:?}"
		);
		assert_eq!(h3.session_error(), None);

		let raw = CloseReason::new(true);
		assert_eq!(raw.map(closed(7)).session_error(), Some((7, "bye".into())));
	}
}
