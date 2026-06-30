/// Errors produced by the WebRTC <-> MoQ gateway.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("invalid SDP: {0}")]
	InvalidSdp(String),

	#[error("unsupported codec: {0}")]
	UnsupportedCodec(String),

	#[error("session not found")]
	SessionNotFound,

	#[error("session closed")]
	SessionClosed,

	#[error("ICE did not connect before the establishment deadline")]
	IceTimeout,

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("moq error: {0}")]
	Moq(#[from] moq_net::Error),

	#[error("mux error: {0}")]
	Mux(#[from] moq_mux::Error),

	#[error("rtc error: {0}")]
	Rtc(#[from] str0m::RtcError),

	#[error("rtc input error: {0}")]
	RtcInput(#[from] str0m::error::NetError),

	#[error(transparent)]
	Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
