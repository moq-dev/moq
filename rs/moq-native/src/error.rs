/// Errors produced while configuring or establishing native MoQ connections.
///
/// Backend-specific failures live in per-backend error types ([`crate::tls::Error`],
/// [`crate::quinn::Error`], etc.) and are composed in here via `#[from]`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error(transparent)]
	MoqNet(#[from] moq_net::Error),

	#[error("invalid log directive")]
	Directive(#[from] tracing_subscriber::filter::ParseError),

	#[error("failed to set global tracing subscriber")]
	SetSubscriber(#[source] tracing_subscriber::util::TryInitError),

	#[error("failed to initialize Android logcat layer")]
	Logcat(#[source] std::io::Error),

	#[error("{0}")]
	NoBackend(&'static str),

	#[error("failed to connect to server")]
	ConnectFailed,

	#[cfg(feature = "iroh")]
	#[error("Iroh support is not enabled")]
	IrohDisabled,

	#[error("tls.root (mTLS) is only supported by the quinn backend")]
	MtlsQuinnOnly,

	#[error("invalid status code")]
	InvalidStatusCode,

	#[error("{0}")]
	Reconnect(String),

	#[error(transparent)]
	Tls(#[from] crate::tls::Error),

	#[cfg(feature = "quinn")]
	#[error(transparent)]
	Quinn(#[from] crate::quinn::Error),

	#[cfg(feature = "noq")]
	#[error(transparent)]
	Noq(#[from] crate::noq::Error),

	#[cfg(feature = "quiche")]
	#[error(transparent)]
	Quiche(#[from] crate::quiche::Error),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	Iroh(#[from] crate::iroh::Error),

	#[cfg(feature = "websocket")]
	#[error(transparent)]
	WebSocket(#[from] crate::websocket::Error),
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;
