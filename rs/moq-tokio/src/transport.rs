//! The network transport carrying a MoQ session.

/// The network transport carrying a MoQ session, on either side of it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Transport {
	/// Raw QUIC, negotiating a MoQ ALPN directly.
	Quic,
	/// A WebSocket connection using qmux framing.
	WebSocket,
	/// A plaintext TCP connection using qmux framing.
	Tcp,
	/// A Unix domain socket using qmux framing.
	Unix,
	/// WebTransport over HTTP/3 on QUIC.
	WebTransport,
	/// A WebRTC data channel using qmux framing.
	WebRtc,
}

impl Transport {
	/// Returns the stable lowercase name used in logs and external metadata.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Quic => "quic",
			Self::WebSocket => "websocket",
			Self::Tcp => "tcp",
			Self::Unix => "unix",
			Self::WebTransport => "webtransport",
			Self::WebRtc => "webrtc",
		}
	}
}

impl std::fmt::Display for Transport {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}
