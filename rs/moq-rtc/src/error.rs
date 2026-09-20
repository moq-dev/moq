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

	/// A broadcast did not produce a catalog before the negotiation deadline.
	#[error("catalog did not arrive before the negotiation deadline")]
	CatalogTimeout,

	/// The catalog track closed before publishing its first snapshot.
	#[error("catalog closed before its first snapshot")]
	CatalogClosed,

	/// The catalog has no rendition this gateway can send over WebRTC.
	#[error("catalog has no WebRTC-compatible renditions")]
	NoRenditions,

	/// The WebRTC engine produced no SDP changes for an offer.
	#[error("no SDP changes to apply")]
	NoSdpChanges,

	/// I/O error on the media socket (bind, send, or receive).
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	/// Error from the underlying moq-net transport.
	#[error("moq error: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the moq-mux import/export layer bridging RTP and MoQ.
	#[error("mux error: {0}")]
	Mux(#[from] moq_mux::Error),

	/// HTTP transport failed while dialing a WHIP or WHEP endpoint.
	#[error("http error: {0}")]
	Http(std::sync::Arc<reqwest::Error>),

	/// A WHIP or WHEP endpoint rejected the HTTP request.
	#[error("HTTP endpoint returned status {0}")]
	HttpStatus(u16),

	/// Error from the WebRTC engine (SDP negotiation, DTLS, media state).
	#[error("rtc error: {0}")]
	Rtc(String),

	/// Error feeding a received UDP datagram into the WebRTC engine.
	#[error("rtc input error: {0}")]
	RtcInput(String),

	/// An internal video bridge could not be initialized.
	#[error("video bridge initialization failed")]
	BridgeFailed,
}

impl From<reqwest::Error> for Error {
	fn from(err: reqwest::Error) -> Self {
		Self::Http(std::sync::Arc::new(err))
	}
}

impl Error {
	pub(crate) fn rtc(err: impl std::fmt::Display) -> Self {
		Self::Rtc(err.to_string())
	}

	pub(crate) fn rtc_input(err: impl std::fmt::Display) -> Self {
		Self::RtcInput(err.to_string())
	}
}

/// Convenience alias for results from the WebRTC <-> MoQ gateway.
pub type Result<T> = std::result::Result<T, Error>;
