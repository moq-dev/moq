/// Errors produced by the WebRTC <-> MoQ gateway.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// An SDP offer or answer failed to parse or was missing something required.
	#[error("invalid SDP: {0}")]
	InvalidSdp(String),

	/// The negotiated payload used a codec this gateway can't bridge to MoQ.
	#[error("unsupported codec: {0}")]
	UnsupportedCodec(String),

	/// No live session matched the resource id (e.g. a DELETE for an unknown id).
	#[error("session not found")]
	SessionNotFound,

	/// The peer closed the session; the media session ended without a failure.
	#[error("session closed")]
	SessionClosed,

	/// ICE connectivity was not established before the establishment deadline.
	#[error("ICE did not connect before the establishment deadline")]
	IceTimeout,

	/// I/O error on the media socket (bind, send, or receive).
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	/// Error from the underlying moq-net transport.
	#[error("moq error: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the moq-mux import/export layer bridging RTP and MoQ.
	#[error("mux error: {0}")]
	Mux(#[from] moq_mux::Error),

	/// Error from the str0m WebRTC engine (SDP negotiation, DTLS, media state).
	#[error("rtc error: {0}")]
	Rtc(#[from] str0m::RtcError),

	/// Error feeding a received UDP datagram into the str0m WebRTC engine.
	#[error("rtc input error: {0}")]
	RtcInput(#[from] str0m::error::NetError),

	/// Catch-all for gateway logic that reports via `anyhow`.
	#[error(transparent)]
	Other(#[from] anyhow::Error),
}

/// Convenience alias for results from the WebRTC <-> MoQ gateway.
pub type Result<T> = std::result::Result<T, Error>;
