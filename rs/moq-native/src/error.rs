use std::sync::Arc;

/// Errors produced while configuring or establishing native MoQ connections.
///
/// Backend-specific failures live in per-backend error types ([`crate::tls::Error`],
/// the per-backend `Error` types, etc.). They're wrapped in `Arc` here so the aggregate
/// stays `Clone` even though the underlying transport/IO errors are not.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Reading or writing a socket, certificate, or key file failed.
	#[error(transparent)]
	Io(Arc<std::io::Error>),

	/// The MoQ session itself failed, after the transport was established.
	#[error(transparent)]
	MoqNet(#[from] moq_net::Error),

	/// The log filter string (ex. `RUST_LOG`) isn't a valid tracing directive.
	#[error("invalid log directive")]
	Directive(#[source] Arc<tracing_subscriber::filter::ParseError>),

	/// Logging was initialized twice, or something else already claimed the global subscriber.
	#[error("failed to set global tracing subscriber")]
	SetSubscriber(#[source] Arc<tracing_subscriber::util::TryInitError>),

	/// Logging couldn't attach to Android's logcat.
	#[error("failed to initialize Android logcat layer")]
	Logcat(#[source] Arc<std::io::Error>),

	/// No backend feature is compiled in that can serve this URL. The string names the features to enable.
	#[error("{0}")]
	NoBackend(&'static str),

	/// Every backend we tried gave up without reporting why.
	#[error("failed to connect to server")]
	ConnectFailed,

	/// The server rejected the connection with an auth status. See [`crate::ConnectError`].
	#[error(transparent)]
	Connect(#[from] crate::ConnectError),

	/// Both halves of the QUIC/WebSocket race failed, so neither error alone tells the story.
	#[cfg(feature = "websocket")]
	#[error("failed to connect to server: QUIC failed: {quic}; WebSocket failed: {websocket}")]
	TransportRace {
		/// Why the QUIC attempt failed.
		quic: Arc<Error>,
		/// Why the WebSocket attempt failed.
		websocket: Arc<Error>,
	},

	/// An `iroh://` URL was dialed but the client was built without an Iroh endpoint.
	#[cfg(feature = "iroh")]
	#[error("Iroh support is not enabled")]
	IrohDisabled,

	/// A client certificate was configured, but this QUIC backend can't do mTLS.
	#[error("tls.root (mTLS) is not supported by the selected QUIC backend")]
	MtlsUnsupported,

	/// The server's WebTransport response carried a status outside the valid HTTP range.
	#[error("invalid status code")]
	InvalidStatusCode,

	/// Reconnecting gave up, usually after the backoff timeout expired. The string has the details.
	#[error("{0}")]
	Reconnect(String),

	/// Loading certificates or building the TLS config failed.
	#[error(transparent)]
	Tls(Arc<crate::tls::Error>),

	/// The Quinn backend failed.
	#[cfg(feature = "quinn")]
	#[error(transparent)]
	Quinn(Arc<crate::quinn::Error>),

	/// The noq backend failed.
	#[cfg(feature = "noq")]
	#[error(transparent)]
	Noq(Arc<crate::noq::Error>),

	/// The quiche backend failed.
	#[cfg(feature = "quiche")]
	#[error(transparent)]
	Quiche(Arc<crate::quiche::Error>),

	/// The Iroh backend failed.
	#[cfg(feature = "iroh")]
	#[error(transparent)]
	Iroh(Arc<crate::iroh::Error>),

	/// The WebSocket fallback transport failed.
	#[cfg(feature = "websocket")]
	#[error(transparent)]
	WebSocket(Arc<crate::websocket::Error>),

	/// The TCP (qmux) transport failed.
	#[cfg(feature = "tcp")]
	#[error(transparent)]
	Tcp(Arc<crate::tcp::Error>),

	/// The Unix socket transport failed.
	#[cfg(all(feature = "uds", unix))]
	#[error(transparent)]
	Unix(Arc<crate::unix::Error>),
}

impl Error {
	/// The auth rejection behind this error, digging through backend and race variants.
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

	/// True if the server rejected us for auth reasons, so retrying won't help without new credentials.
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

#[cfg(feature = "tcp")]
impl From<crate::tcp::Error> for Error {
	fn from(err: crate::tcp::Error) -> Self {
		Self::Tcp(Arc::new(err))
	}
}

#[cfg(all(feature = "uds", unix))]
impl From<crate::unix::Error> for Error {
	fn from(err: crate::unix::Error) -> Self {
		Self::Unix(Arc::new(err))
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
