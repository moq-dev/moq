//! Why a WebTransport (or raw QUIC) session failed.

/// Why a WebTransport (or raw QUIC) session failed: its handshake, its connection, or a close.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The QUIC connection failed underneath the session.
	#[error("connection error: {0}")]
	Connection(#[from] crate::quic::Error),

	/// The session closed with this application code and reason.
	#[error("closed: code={0} reason={1}")]
	Closed(u32, String),

	/// The peer tore down the session's streams before its close reason arrived.
	#[error("session gone")]
	SessionGone,

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

	/// A QUIC stream the session reads, such as a handshake stream, failed.
	#[error("read error: {0}")]
	Read(#[from] crate::quic::recv::Error),

	/// A QUIC stream or datagram the session writes failed.
	#[error("write error: {0}")]
	Write(#[from] crate::quic::send::Error),
}

/// Why the HTTP/3 SETTINGS or CONNECT exchange failed.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HandshakeError {
	/// A handshake stream ended before its message did.
	#[error("stream ended early")]
	UnexpectedEnd,

	/// A handshake message outgrew the buffer limit.
	#[error("message too large")]
	TooLarge,

	/// The peer's SETTINGS do not enable WebTransport.
	#[error("WebTransport is not supported")]
	Unsupported,

	/// The SETTINGS were malformed.
	#[error("settings: {0}")]
	Settings(#[from] web_transport_proto::SettingsError),

	/// The CONNECT request was malformed, or the response could not be encoded.
	#[error("connect: {0}")]
	Connect(#[from] web_transport_proto::ConnectError),

	/// A status that does not fit the answer: accepting with anything but 200, or rejecting
	/// with one below 400.
	#[error("http status: {0}")]
	Status(http::StatusCode),

	/// The response selected a subprotocol the request did not offer.
	#[error("subprotocol not offered: {0}")]
	Protocol(String),
}

impl Error {
	/// The QUIC connection error this one carries, if the connection failing caused it.
	fn connection(&self) -> Option<&crate::quic::Error> {
		match self {
			Self::Connection(err)
			| Self::Read(crate::quic::recv::Error::Connection(err))
			| Self::Write(crate::quic::send::Error::Connection(err)) => Some(err),
			_ => None,
		}
	}

	/// Decode a peer's close code once the connection, not the session, reported it: in
	/// WebTransport code space for `web`, as is for raw QUIC.
	pub(super) fn decode(self, web: bool) -> Self {
		let Some(crate::quic::Error::App { code, reason }) = self.connection() else {
			return self;
		};
		let (code, reason) = (*code, reason.clone());
		if web {
			return match web_transport_proto::error_from_http3(code) {
				Some(code) => Self::Closed(code, reason),
				None => Self::Http3 { code, reason },
			};
		}
		match u32::try_from(code) {
			Ok(code) => Self::Closed(code, reason),
			Err(_) => self,
		}
	}

	/// Whether the session's recorded close reason should replace this error: the connection
	/// closing caused it, or it is a close itself.
	pub(super) fn is_close(&self) -> bool {
		self.connection().is_some() || matches!(self, Self::Closed(..) | Self::SessionGone | Self::Http3 { .. })
	}
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		match self.connection() {
			Some(err) => err.close_kind(),
			None => moq_net::transport::CloseKind::Transport,
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
		// Raw QUIC writes no header, so the code is always in HTTP/3 space here.
		match self {
			Self::Write(crate::quic::send::Error::Stopped(code)) => web_transport_proto::error_from_http3(code.into_inner()),
			_ => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::transport::Error as _;

	/// A peer close maps into WebTransport code space, or is HTTP/3's own failure when its
	/// code names none, and raw QUIC passes the code through.
	#[test]
	fn a_peer_close_decodes_by_mode() {
		let closed = |code: u64| {
			Error::Connection(crate::quic::Error::App {
				code,
				reason: "bye".into(),
			})
		};

		let mapped = closed(web_transport_proto::error_to_http3(7)).decode(true);
		assert_eq!(mapped.session_error(), Some((7, "bye".into())));

		// H3_NO_ERROR: HTTP/3's, and no WebTransport code at all.
		let h3 = closed(0x100).decode(true);
		assert!(
			matches!(&h3, Error::Http3 { code: 0x100, reason } if reason == "bye"),
			"got {h3:?}"
		);
		assert_eq!(h3.session_error(), None, "not a code the peer's application chose");

		assert_eq!(closed(7).decode(false).session_error(), Some((7, "bye".into())));
		let outside = closed(u64::MAX).decode(false);
		assert!(matches!(outside, Error::Connection(_)), "got {outside:?}");
		assert_eq!(outside.session_error(), None);
	}

	/// A stream code at the session level is reported only from a header write the peer
	/// stopped, and only when it names a WebTransport code.
	#[test]
	fn header_stops_keep_their_registry() {
		let stopped = |code: u64| Error::Write(crate::quic::send::Error::Stopped(crate::quic::VarInt::from_u64(code).unwrap()));
		assert_eq!(stopped(web_transport_proto::error_to_http3(3)).stream_error(), Some(3));
		assert_eq!(stopped(0x100).stream_error(), None, "not a MoQ stream code");
	}
}
