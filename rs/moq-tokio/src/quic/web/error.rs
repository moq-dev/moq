use std::sync::{Arc, OnceLock};

use thiserror::Error;

use crate::quic::web::{ConnectError, SettingsError};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum ClientError {
	/// The peer ended the handshake before it was complete.
	#[error("unexpected end of stream")]
	UnexpectedEnd,

	/// The QUIC connection failed.
	#[error("connection error: {0}")]
	Connection(#[from] crate::quic::driver::connection::Error),

	/// The handshake could not write its stream.
	#[error("failed to write: {0}")]
	WriteError(#[from] crate::quic::driver::send::Error),

	/// The handshake could not read its stream.
	#[error("failed to read: {0}")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	/// The HTTP/3 SETTINGS exchange failed.
	#[error("failed to exchange h3 settings: {0}")]
	SettingsError(#[from] SettingsError),

	/// The HTTP/3 CONNECT exchange failed.
	#[error("failed to exchange h3 connect: {0}")]
	HttpError(#[from] ConnectError),

	/// The QUIC handshake could not start.
	#[error("quic error: {0}")]
	ConnectError(#[from] crate::quic::driver::endpoint::ConnectError),
}

/// An errors returned by [`crate::quic::web::Session`], split based on if they are underlying QUIC errors or
/// WebTransport errors.
#[derive(Clone, Error, Debug)]
#[non_exhaustive]
pub enum SessionError {
	/// The QUIC connection failed.
	#[error("connection error: {0}")]
	ConnectionError(crate::quic::driver::connection::Error),

	/// The WebTransport session failed.
	#[error("webtransport error: {0}")]
	WebTransportError(#[from] WebTransportError),

	/// The connection refused the datagram.
	#[error("send datagram error: {0}")]
	SendDatagramError(#[from] crate::quic::driver::send::Error),
}

impl From<crate::quic::driver::connection::Error> for SessionError {
	fn from(e: crate::quic::driver::connection::Error) -> Self {
		match &e {
			crate::quic::driver::connection::Error::ApplicationClosed(close) => {
				match web_transport_proto::error_from_http3(close.error_code.into_inner()) {
					Some(code) => WebTransportError::Closed(code, close.reason.clone()).into(),
					None => SessionError::ConnectionError(e),
				}
			}
			_ => SessionError::ConnectionError(e),
		}
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
	reason: OnceLock<SessionError>,
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
	/// unknown: [`WebTransportError::SessionGone`], never a guessed clean close.
	pub(crate) fn session_gone(&self, code: crate::quic::driver::VarInt) -> Option<SessionError> {
		if self.raw || code.into_inner() != WT_SESSION_GONE {
			return None;
		}
		Some(
			self.reason
				.get()
				.cloned()
				.unwrap_or_else(|| WebTransportError::SessionGone.into()),
		)
	}

	/// Record the close reason, returning false if one was already recorded.
	pub(crate) fn set(&self, err: SessionError) -> bool {
		self.reason.set(err).is_ok()
	}

	/// Replace an error caused by the connection closing with the recorded reason, or
	/// decode the peer's raw QUIC close code when none was recorded.
	pub(crate) fn map(&self, err: SessionError) -> SessionError {
		let conn = match &err {
			SessionError::ConnectionError(conn)
			| SessionError::SendDatagramError(crate::quic::driver::send::Error::Connection(conn)) => Some(conn),
			SessionError::WebTransportError(WebTransportError::Closed(..) | WebTransportError::SessionGone) => None,
			_ => return err,
		};

		if let Some(reason) = self.reason.get() {
			return reason.clone();
		}

		match conn {
			Some(crate::quic::driver::connection::Error::ApplicationClosed(close)) if self.raw => {
				match u32::try_from(close.error_code.into_inner()) {
					Ok(code) => WebTransportError::Closed(code, close.reason.clone()).into(),
					Err(_) => err,
				}
			}
			_ => err,
		}
	}
}

/// An error that can occur when reading/writing the WebTransport stream header.
#[derive(Clone, Error, Debug)]
#[non_exhaustive]
pub enum WebTransportError {
	/// The peer closed the session with its application code and reason.
	#[error("closed: code={0} reason={1}")]
	Closed(u32, String),

	/// The stream or datagram named a different session.
	#[error("unknown session")]
	UnknownSession,

	/// The peer tore down the session's streams before its close reason arrived.
	#[error("session gone")]
	SessionGone,

	/// The QUIC receive stream failed.
	#[error("read error: {0}")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	/// The QUIC send stream failed.
	#[error("write error: {0}")]
	WriteError(#[from] crate::quic::driver::send::Error),
}

/// An error returned when receiving a new WebTransport session.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum ServerError {
	/// The peer ended the handshake before it was complete.
	#[error("unexpected end of stream")]
	UnexpectedEnd,

	/// The QUIC connection failed.
	#[error("connection error")]
	Connection(#[from] crate::quic::driver::connection::Error),

	/// The handshake could not write its stream.
	#[error("failed to write")]
	WriteError(#[from] crate::quic::driver::send::Error),

	/// The handshake could not read its stream.
	#[error("failed to read")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	/// The HTTP/3 SETTINGS exchange failed.
	#[error("failed to exchange h3 settings")]
	SettingsError(#[from] SettingsError),

	/// The HTTP/3 CONNECT exchange failed.
	#[error("failed to exchange h3 connect")]
	ConnectError(#[from] ConnectError),

	/// The endpoint socket failed.
	#[error("io error: {0}")]
	IoError(Arc<std::io::Error>),

	#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
	/// The TLS configuration was invalid.
	#[error("rustls error: {0}")]
	Rustls(#[from] rustls::Error),
}

impl moq_net::transport::Error for SessionError {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		use crate::quic::driver::connection::Error;
		match self {
			Self::ConnectionError(Error::TimedOut) => moq_net::transport::CloseKind::IdleTimeout,
			Self::ConnectionError(Error::LocallyClosed { .. }) => moq_net::transport::CloseKind::Closed,
			_ => moq_net::transport::CloseKind::Transport,
		}
	}
	fn session_error(&self) -> Option<(u32, String)> {
		if let SessionError::WebTransportError(WebTransportError::Closed(code, reason)) = self {
			return Some((*code, reason.to_string()));
		}

		None
	}
	fn stream_error(&self) -> Option<u32> {
		// An open can fail while writing its WebTransport header, before it returns a stream.
		if let Self::WebTransportError(WebTransportError::WriteError(crate::quic::driver::send::Error::Stopped(code))) =
			self
		{
			return web_transport_proto::error_from_http3(code.into_inner());
		}
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn transport_error_kinds_preserve_stream_and_session_registries() {
		use moq_net::transport::{CloseKind, Error as _};
		let idle = SessionError::ConnectionError(crate::quic::driver::connection::Error::TimedOut);
		assert_eq!(idle.close_kind(), CloseKind::IdleTimeout);
		assert_eq!(
			super::super::recv::Error::SessionError(idle.clone()).close_kind(),
			CloseKind::IdleTimeout
		);
		assert_eq!(
			super::super::send::Error::SessionError(idle).close_kind(),
			CloseKind::IdleTimeout
		);
		assert_eq!(super::super::recv::Error::Evicted.close_kind(), CloseKind::Evicted);
		let local = SessionError::ConnectionError(crate::quic::driver::connection::Error::LocallyClosed {
			code: 7u32.into(),
			reason: String::new(),
		});
		assert_eq!(local.close_kind(), CloseKind::Closed);
		let session = SessionError::WebTransportError(WebTransportError::Closed(7, "session".into()));
		assert_eq!(session.session_error(), Some((7, "session".into())));
		assert_eq!(session.stream_error(), None);
		let stream = super::super::recv::Error::Reset(7);
		assert_eq!(stream.session_error(), None);
		assert_eq!(stream.stream_error(), Some(7));
		let close = CloseReason::new(false);
		let header = SessionError::WebTransportError(WebTransportError::WriteError(
			crate::quic::driver::send::Error::Stopped(close.encode_stream_code(7)),
		));
		assert_eq!(header.stream_error(), Some(7));
		let invalid = SessionError::WebTransportError(WebTransportError::WriteError(
			crate::quic::driver::send::Error::Stopped(7u32.into()),
		));
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
		assert!(matches!(
			close.session_gone(gone),
			Some(SessionError::WebTransportError(WebTransportError::SessionGone))
		));

		close.set(WebTransportError::Closed(7, "bye".into()).into());
		assert!(matches!(
			close.session_gone(gone),
			Some(SessionError::WebTransportError(WebTransportError::Closed(7, reason))) if reason == "bye"
		));
		assert!(close.session_gone(close.encode_stream_code(7)).is_none());

		// Raw QUIC has no HTTP/3 code space, so the same number is an application code.
		assert!(CloseReason::new(true).session_gone(gone).is_none());
	}
}
