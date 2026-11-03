use std::sync::{Arc, OnceLock};

use thiserror::Error;

use crate::quic::web::{ConnectError, SettingsError};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum ClientError {
	#[error("unexpected end of stream")]
	UnexpectedEnd,

	#[error("connection error: {0}")]
	Connection(#[from] crate::quic::driver::connection::Error),

	#[error("failed to write: {0}")]
	WriteError(#[from] crate::quic::driver::send::Error),

	#[error("failed to read: {0}")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	#[error("failed to exchange h3 settings: {0}")]
	SettingsError(#[from] SettingsError),

	#[error("failed to exchange h3 connect: {0}")]
	HttpError(#[from] ConnectError),

	#[error("quic error: {0}")]
	ConnectError(#[from] crate::quic::driver::endpoint::ConnectError),
}

/// An errors returned by [`crate::quic::web::Session`], split based on if they are underlying QUIC errors or
/// WebTransport errors.
#[derive(Clone, Error, Debug)]
#[non_exhaustive]
pub enum SessionError {
	#[error("connection error: {0}")]
	ConnectionError(crate::quic::driver::connection::Error),

	#[error("webtransport error: {0}")]
	WebTransportError(#[from] WebTransportError),

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
	#[error("closed: code={0} reason={1}")]
	Closed(u32, String),

	#[error("unknown session")]
	UnknownSession,

	/// The peer tore down the session's streams before its close reason arrived.
	#[error("session gone")]
	SessionGone,

	#[error("read error: {0}")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	#[error("write error: {0}")]
	WriteError(#[from] crate::quic::driver::send::Error),
}

/// An error when writing to [`crate::quic::web::SendStream`]. Similar to [`crate::quic::driver::send::Error`].
#[derive(Clone, Error, Debug)]
#[non_exhaustive]
pub enum WriteError {
	#[error("STOP_SENDING: {0}")]
	Stopped(u32),

	#[error("invalid STOP_SENDING: {0}")]
	InvalidStopped(crate::quic::driver::VarInt),

	#[error("session error: {0}")]
	SessionError(#[from] SessionError),

	/// The stream was already finished or reset.
	#[error("stream closed")]
	ClosedStream,
}

impl From<crate::quic::driver::send::Error> for WriteError {
	fn from(e: crate::quic::driver::send::Error) -> Self {
		match e {
			crate::quic::driver::send::Error::Stopped(code) => {
				match web_transport_proto::error_from_http3(code.into_inner()) {
					Some(code) => WriteError::Stopped(code),
					None => WriteError::InvalidStopped(code),
				}
			}
			crate::quic::driver::send::Error::Connection(e) => WriteError::SessionError(e.into()),
			// Only datagrams fail otherwise, and they report a session error.
			e => WriteError::SessionError(e.into()),
		}
	}
}

/// An error when reading from [`crate::quic::web::RecvStream`]. Similar to [`crate::quic::driver::recv::Error`].
#[derive(Clone, Error, Debug)]
#[non_exhaustive]
pub enum ReadError {
	#[error("session error: {0}")]
	SessionError(#[from] SessionError),

	#[error("RESET_STREAM: {0}")]
	Reset(u32),

	#[error("invalid RESET_STREAM: {0}")]
	InvalidReset(crate::quic::driver::VarInt),

	/// The stream was already stopped.
	#[error("stream already closed")]
	ClosedStream,

	/// The connection's receive window was stuck and this stream was discarded to free it.
	#[error("evicted")]
	Evicted,

	/// The stream finished before [`read_exact`](crate::quic::web::RecvStream::read_exact)
	/// filled its buffer, after this many bytes.
	#[error("finished early")]
	FinishedEarly(usize),

	/// The stream is longer than [`read_to_end`](crate::quic::web::RecvStream::read_to_end)'s limit.
	#[error("too long")]
	TooLong,

	/// A lease wanted more than the connection receive window can hold.
	#[error("want exceeds the receive window")]
	TooLarge,
}

impl From<crate::quic::driver::recv::Error> for ReadError {
	fn from(value: crate::quic::driver::recv::Error) -> Self {
		use crate::quic::driver::recv::Error;
		match value {
			Error::Reset(code) => match web_transport_proto::error_from_http3(code.into_inner()) {
				Some(code) => ReadError::Reset(code),
				None => ReadError::InvalidReset(code),
			},
			Error::Connection(e) => ReadError::SessionError(e.into()),
			Error::Evicted => ReadError::Evicted,
			Error::FinishedEarly(n) => ReadError::FinishedEarly(n),
			Error::TooLong => ReadError::TooLong,
			Error::TooLarge => ReadError::TooLarge,
		}
	}
}

/// An error returned when receiving a new WebTransport session.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum ServerError {
	#[error("unexpected end of stream")]
	UnexpectedEnd,

	#[error("connection error")]
	Connection(#[from] crate::quic::driver::connection::Error),

	#[error("failed to write")]
	WriteError(#[from] crate::quic::driver::send::Error),

	#[error("failed to read")]
	ReadError(#[from] crate::quic::driver::recv::Error),

	#[error("failed to exchange h3 settings")]
	SettingsError(#[from] SettingsError),

	#[error("failed to exchange h3 connect")]
	ConnectError(#[from] ConnectError),

	#[error("io error: {0}")]
	IoError(Arc<std::io::Error>),

	#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
	#[error("rustls error: {0}")]
	Rustls(#[from] rustls::Error),
}

impl web_transport_trait::Error for SessionError {
	fn session_error(&self) -> Option<(u32, String)> {
		if let SessionError::WebTransportError(WebTransportError::Closed(code, reason)) = self {
			return Some((*code, reason.to_string()));
		}

		None
	}
}

impl web_transport_trait::Error for WriteError {
	fn session_error(&self) -> Option<(u32, String)> {
		if let WriteError::SessionError(e) = self {
			return e.session_error();
		}

		None
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			WriteError::Stopped(code) => Some(*code),
			_ => None,
		}
	}
}

impl web_transport_trait::Error for ReadError {
	fn session_error(&self) -> Option<(u32, String)> {
		if let ReadError::SessionError(e) = self {
			return e.session_error();
		}

		None
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			ReadError::Reset(code) => Some(*code),
			_ => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
