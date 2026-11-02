use std::sync::Arc;

use moq_net::transport::CloseKind;
use moq_quic::{LimitsError, TransportErrorCode, connection::Error as ConnectionError};

/// Errors that can occur during QMux session and stream operations.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// The stream already finished or was reset.
	#[error("stream closed")]
	StreamClosed,

	/// The session ended with an application close: the peer's APPLICATION_CLOSE, or our
	/// own [`close`](moq_net::transport::poll::Session::close).
	#[error("connection closed: {code}: {reason}")]
	ConnectionClosed { code: u64, reason: String },

	/// The peer sent a CONNECTION_CLOSE: it detected a protocol violation or transport
	/// error. Unlike [`ConnectionClosed`](Self::ConnectionClosed), it does *not* surface as
	/// an application close.
	#[error("connection reset: {code}: {reason}")]
	ConnectionReset { code: u64, reason: String },

	/// The peer reset the stream with this code.
	#[error("stream reset: {0}")]
	StreamReset(u64),

	/// The peer asked us to stop sending on the stream with this code.
	#[error("stream stop: {0}")]
	StreamStop(u64),

	/// A record or frame exceeded its size limit.
	#[error("frame too large")]
	FrameTooLarge,

	/// The peer sent more than its flow control credit allowed.
	#[error("flow control error")]
	FlowControlError,

	/// A frame was malformed on the wire, QUIC's FRAME_ENCODING_ERROR.
	#[error("frame encoding error")]
	FrameEncoding,

	/// The peer broke a protocol rule, QUIC's PROTOCOL_VIOLATION.
	#[error("protocol violation")]
	ProtocolViolation,

	/// A transport parameter carried an illegal value, QUIC's TRANSPORT_PARAMETER_ERROR.
	#[error("transport parameter error")]
	TransportParameter,

	/// A [`Config`](crate::Config) value the session cannot run with.
	#[error("invalid config: {0}")]
	InvalidConfig(&'static str),

	/// The flow-control limits are ones a peer would refuse.
	#[error("invalid limits: {0}")]
	Limits(#[from] LimitsError),

	/// The peer opened more streams than it was allowed.
	#[error("stream limit exceeded")]
	StreamLimitExceeded,

	/// The transport ended without a close frame, or the session was dropped.
	#[error("connection closed")]
	Closed,

	/// The peer went silent for the idle timeout.
	#[error("idle timeout")]
	IdleTimeout,

	/// The peer never sent its transport parameters.
	#[error("handshake timeout: peer never sent transport parameters")]
	HandshakeTimeout,

	/// An application protocol name that is not an HTTP token, or an ALPN that names no
	/// QMux version.
	#[error("invalid protocol token: {0:?}")]
	InvalidProtocol(String),

	/// The TLS server name is not a valid DNS name or IP address.
	#[error("invalid server name")]
	InvalidServerName,

	/// The server rejected the handshake with a non-success HTTP status.
	///
	/// Surfaced from the WebSocket upgrade response so callers can distinguish
	/// terminal failures (e.g. 401/403 auth rejections) from transient I/O
	/// errors worth retrying.
	#[error("http error status: {0}")]
	Http(u16),

	// Foreign error sources aren't `Clone`, but `Error` must be (the session
	// fans one terminal error out to every waiter). `Arc` bridges the two while
	// keeping the original error, source chain and all, instead of a string.
	/// The transport's I/O failed.
	#[error(transparent)]
	Io(Arc<std::io::Error>),

	/// The WebSocket failed.
	#[cfg(feature = "ws")]
	#[error(transparent)]
	WebSocket(Arc<tokio_tungstenite::tungstenite::Error>),

	/// Datagrams are disabled on this session, or the peer does not accept them.
	#[error("datagrams not supported")]
	DatagramsUnsupported,

	/// The datagram exceeds [`max_datagram_size`](moq_net::transport::poll::Session::max_datagram_size).
	#[error("datagram too large")]
	DatagramTooLarge,
}

impl Error {
	/// The terminal error for a connection that ended for `reason`
	pub(crate) fn lost(reason: ConnectionError) -> Self {
		match reason {
			ConnectionError::ApplicationClosed(close) => Self::ConnectionClosed {
				code: close.error_code.into_inner(),
				reason: close.reason,
			},
			ConnectionError::LocallyClosed { code, reason } => Self::ConnectionClosed {
				code: code.into_inner(),
				reason,
			},
			ConnectionError::ConnectionClosed(close) => Self::ConnectionReset {
				code: close.error_code.into(),
				reason: close.reason,
			},
			ConnectionError::TransportError(err) => match err.code {
				TransportErrorCode::FLOW_CONTROL_ERROR | TransportErrorCode::FINAL_SIZE_ERROR => Self::FlowControlError,
				TransportErrorCode::FRAME_ENCODING_ERROR => Self::FrameEncoding,
				TransportErrorCode::TRANSPORT_PARAMETER_ERROR => Self::TransportParameter,
				TransportErrorCode::STREAM_LIMIT_ERROR => Self::StreamLimitExceeded,
				_ => Self::ProtocolViolation,
			},
			ConnectionError::TimedOut => Self::IdleTimeout,
			ConnectionError::HandshakeTimeout => Self::HandshakeTimeout,
			_ => Self::Closed,
		}
	}
}

// Hand-written rather than `#[from]`: thiserror's `#[from]` would generate
// `From<Arc<std::io::Error>>`, but `?` call sites yield a bare `std::io::Error`.
impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Self::Io(Arc::new(err))
	}
}

#[cfg(feature = "ws")]
impl From<tokio_tungstenite::tungstenite::Error> for Error {
	fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
		// A non-101 upgrade response carries the HTTP status; surface it as a
		// distinct, transport-agnostic variant so callers can act on terminal
		// rejections (e.g. auth) without downcasting tungstenite.
		if let tokio_tungstenite::tungstenite::Error::Http(response) = &err {
			return Self::Http(response.status().as_u16());
		}
		Self::WebSocket(Arc::new(err))
	}
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> CloseKind {
		match self {
			Self::IdleTimeout => CloseKind::IdleTimeout,
			Self::Closed => CloseKind::Closed,
			_ => CloseKind::Transport,
		}
	}

	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::ConnectionClosed { code, reason } => Some(((*code).try_into().ok()?, reason.clone())),
			_ => None,
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::StreamReset(code) | Self::StreamStop(code) => (*code).try_into().ok(),
			_ => None,
		}
	}
}

#[cfg(all(test, feature = "ws"))]
mod tests {
	use super::*;
	use tokio_tungstenite::tungstenite::{self, http};

	#[test]
	fn preserves_http_status() {
		let response = http::Response::builder()
			.status(http::StatusCode::UNAUTHORIZED)
			.body(None::<Vec<u8>>)
			.unwrap();
		let err: Error = tungstenite::Error::Http(Box::new(response)).into();
		assert!(matches!(err, Error::Http(401)));
	}

	#[test]
	fn non_http_preserves_source() {
		let err: Error = tungstenite::Error::ConnectionClosed.into();
		assert!(matches!(err, Error::WebSocket(_)));
	}
}
