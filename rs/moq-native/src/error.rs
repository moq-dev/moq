use std::sync::Arc;

/// Errors produced while configuring or establishing native MoQ connections.
///
/// Backend-specific failures live in per-backend error types ([`crate::tls::Error`],
/// [`crate::quinn::Error`], etc.). They're wrapped in `Arc` here so the aggregate
/// stays `Clone` even though the underlying transport/IO errors are not.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error(transparent)]
	Io(Arc<std::io::Error>),

	#[error(transparent)]
	MoqNet(#[from] moq_net::Error),

	#[error("invalid log directive")]
	Directive(#[source] Arc<tracing_subscriber::filter::ParseError>),

	#[error("failed to set global tracing subscriber")]
	SetSubscriber(#[source] Arc<tracing_subscriber::util::TryInitError>),

	#[error("failed to initialize Android logcat layer")]
	Logcat(#[source] Arc<std::io::Error>),

	#[error("{0}")]
	NoBackend(&'static str),

	#[error("failed to connect to server")]
	ConnectFailed,

	#[error(transparent)]
	Connect(#[from] crate::ConnectError),

	#[cfg(feature = "websocket")]
	#[error("failed to connect to server: QUIC failed: {quic}; WebSocket failed: {websocket}")]
	TransportRace { quic: Arc<Error>, websocket: Arc<Error> },

	#[cfg(feature = "iroh")]
	#[error("Iroh support is not enabled")]
	IrohDisabled,

	#[error("tls.root (mTLS) is not supported by the selected QUIC backend")]
	MtlsUnsupported,

	#[error("invalid status code")]
	InvalidStatusCode,

	#[error("{0}")]
	Reconnect(String),

	#[error(transparent)]
	Tls(Arc<crate::tls::Error>),

	#[cfg(feature = "quinn")]
	#[error(transparent)]
	Quinn(Arc<crate::quinn::Error>),

	#[cfg(feature = "noq")]
	#[error(transparent)]
	Noq(Arc<crate::noq::Error>),

	#[cfg(feature = "quiche")]
	#[error(transparent)]
	Quiche(Arc<crate::quiche::Error>),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	Iroh(Arc<crate::iroh::Error>),

	#[cfg(feature = "websocket")]
	#[error(transparent)]
	WebSocket(Arc<crate::websocket::Error>),
}

impl Error {
	pub fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::Connect(err) => Some(*err),
			Self::MoqNet(moq_net::Error::Unauthorized) => Some(crate::ConnectError::Unauthorized),
			#[cfg(feature = "quinn")]
			Self::Quinn(err) => err.connect_error(),
			#[cfg(feature = "noq")]
			Self::Noq(err) => err.connect_error(),
			#[cfg(feature = "quiche")]
			Self::Quiche(err) => err.connect_error(),
			#[cfg(feature = "websocket")]
			Self::TransportRace { quic, websocket } => quic.connect_error().or_else(|| websocket.connect_error()),
			#[cfg(feature = "websocket")]
			Self::WebSocket(err) => err.connect_error(),
			_ => None,
		}
	}

	pub fn is_auth(&self) -> bool {
		self.connect_error().is_some_and(|err| err.is_auth())
	}
}

// The wrapped sources aren't `Clone`, so `#[from]` can't store them behind `Arc`
// directly. These hand-written conversions keep `?` ergonomic at the call sites.
impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Self::Io(Arc::new(err))
	}
}

impl From<tracing_subscriber::filter::ParseError> for Error {
	fn from(err: tracing_subscriber::filter::ParseError) -> Self {
		Self::Directive(Arc::new(err))
	}
}

impl From<crate::tls::Error> for Error {
	fn from(err: crate::tls::Error) -> Self {
		Self::Tls(Arc::new(err))
	}
}

#[cfg(feature = "quinn")]
impl From<crate::quinn::Error> for Error {
	fn from(err: crate::quinn::Error) -> Self {
		if let Some(err) = err.connect_error() {
			return Self::Connect(err);
		}

		Self::Quinn(Arc::new(err))
	}
}

#[cfg(feature = "noq")]
impl From<crate::noq::Error> for Error {
	fn from(err: crate::noq::Error) -> Self {
		if let Some(err) = err.connect_error() {
			return Self::Connect(err);
		}

		Self::Noq(Arc::new(err))
	}
}

#[cfg(feature = "quiche")]
impl From<crate::quiche::Error> for Error {
	fn from(err: crate::quiche::Error) -> Self {
		if let Some(err) = err.connect_error() {
			return Self::Connect(err);
		}

		Self::Quiche(Arc::new(err))
	}
}

#[cfg(feature = "iroh")]
impl From<crate::iroh::Error> for Error {
	fn from(err: crate::iroh::Error) -> Self {
		Self::Iroh(Arc::new(err))
	}
}

#[cfg(feature = "websocket")]
impl From<crate::websocket::Error> for Error {
	fn from(err: crate::websocket::Error) -> Self {
		if let Some(err) = err.connect_error() {
			return Self::Connect(err);
		}

		Self::WebSocket(Arc::new(err))
	}
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(all(test, feature = "websocket"))]
mod tests {
	use super::*;

	#[test]
	fn transport_race_propagates_nested_connect_errors() {
		let quic = Error::TransportRace {
			quic: Arc::new(crate::ConnectError::Unauthorized.into()),
			websocket: Arc::new(crate::ConnectError::Forbidden.into()),
		};
		assert_eq!(quic.connect_error(), Some(crate::ConnectError::Unauthorized));

		let websocket = Error::TransportRace {
			quic: Arc::new(Error::ConnectFailed),
			websocket: Arc::new(crate::ConnectError::Forbidden.into()),
		};
		assert_eq!(websocket.connect_error(), Some(crate::ConnectError::Forbidden));
	}
}
