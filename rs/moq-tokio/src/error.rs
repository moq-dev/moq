use std::sync::Arc;

/// Renders an error and its `source()` chain into a single message.
///
/// Dependency errors are stored as messages so their crates stay out of this crate's public
/// API. Several of them (reqwest above all) keep the useful detail in `source()`, which a
/// plain `to_string()` would drop.
// A build with no transport feature compiles no backend module, so nothing calls this.
#[allow(dead_code)]
pub(crate) fn message(err: impl std::error::Error) -> String {
	use std::fmt::Write;

	let mut out = err.to_string();
	let mut source = err.source();
	while let Some(err) = source {
		let _ = write!(out, ": {err}");
		source = err.source();
	}
	out
}

/// Generates `From` conversions into message-carrying variants of the enclosing module's `Error`,
/// so `?` still works on a dependency's error without that dependency reaching our public API.
#[allow(unused_macros)]
macro_rules! from_message {
	($($ty:ty => $variant:ident),* $(,)?) => {
		$(
			impl From<$ty> for Error {
				fn from(err: $ty) -> Self {
					Self::$variant($crate::error::message(err))
				}
			}
		)*
	};
}

#[allow(unused_imports)]
pub(crate) use from_message;

/// Whether an HTTP response status means "ask again later".
///
/// A response that arrived is the server's answer, and only this narrow set invites another
/// attempt: request timeout, rate limit, and the gateway/overload statuses. Every other status,
/// `404` and `403` included, is settled.
pub(crate) fn status_retryable(status: u16) -> bool {
	matches!(status, 408 | 429 | 502 | 503 | 504)
}

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
	#[error("invalid log directive: {0}")]
	Directive(String),

	/// Logging was initialized twice, or something else already claimed the global subscriber.
	#[error("failed to set global tracing subscriber: {0}")]
	SetSubscriber(String),

	/// Logging couldn't attach to Android's logcat.
	#[error("failed to initialize Android logcat layer")]
	Logcat(#[source] Arc<std::io::Error>),

	/// No backend feature is compiled in that can serve this URL. The string names the features to enable.
	#[error("{0}")]
	NoBackend(&'static str),

	/// A qlog directory was configured but this build can't capture traces.
	#[error("qlog capture requires the 'qlog' feature")]
	QlogUnsupported,

	/// The config was parsed from released spellings that no longer work. The
	/// payload is the migration to print.
	#[error("{0}")]
	Deprecated(crate::Deprecated),

	/// The idle timeout is longer than QUIC's millisecond varint can carry.
	#[error("idle timeout must be under 2^62 milliseconds")]
	IdleTimeoutRange,

	/// Every backend we tried gave up without reporting why.
	#[error("failed to connect to server")]
	ConnectFailed,

	/// The dial and handshake together outlived the connect timeout.
	///
	/// Not every transport bounds its own dial: QUIC gives up on its own, but a peer
	/// that completes the TCP handshake and then never speaks leaves the WebSocket
	/// fallback (and the MoQ handshake that follows either transport) pending with
	/// nothing to time it out. This deadline turns that into an error the caller can
	/// retry instead of a wait that never ends.
	#[error("connect timed out after {0:?}")]
	ConnectTimeout(std::time::Duration),

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

	/// A QUIC-LB nonce length was set without the server id it encodes alongside.
	#[error("--listen-quic-lb-nonce needs --listen-quic-lb-id")]
	LbNonceWithoutId,

	/// A worker group was asked for more members than the connection ID's one-byte
	/// steering prefix can name.
	#[error("QUIC workers cannot exceed {max}; {count} were requested")]
	WorkerCount {
		/// What was asked for.
		count: u16,
		/// The ceiling, set by the byte the steering filter reads.
		max: u16,
	},

	/// A worker group was asked to generate its own certificate, which would give
	/// every member a different one.
	#[error("QUIC workers cannot generate certificates; configure a certificate and key instead")]
	WorkerTlsGenerate,

	/// A worker group was pointed at an ephemeral port, so each member bound a
	/// port of its own instead of sharing one.
	#[error("QUIC workers need an explicit non-zero listen port; worker {index} bound {addr} instead of {first}")]
	WorkerPortMismatch {
		/// The member that disagreed.
		index: u16,
		/// What it bound.
		addr: std::net::SocketAddr,
		/// What the first member bound, which the rest must match.
		first: std::net::SocketAddr,
	},

	/// Another worker group already holds this listen port.
	///
	/// A second group on an overlapping address would silently join the first's
	/// `SO_REUSEPORT` group and break its steering, so the port is locked while
	/// the first is alive, whatever the address: a group on a different address
	/// sharing the port is refused too.
	#[error("another QUIC worker group already holds port {}", addr.port())]
	WorkerOverlap {
		/// The address this group asked for.
		addr: std::net::SocketAddr,
	},

	/// The worker group's listen address did not resolve.
	///
	/// Resolved once for the whole group, so a DNS answer that rotates between
	/// queries cannot hand members different addresses.
	#[error("QUIC workers failed to resolve the listen address")]
	WorkerResolve(#[source] Arc<std::io::Error>),

	/// A worker thread could not be spawned, or died before it finished binding.
	#[error("QUIC worker {index} failed to start")]
	WorkerStart {
		/// The member that failed.
		index: u16,
		/// Why, when the thread got far enough to say.
		#[source]
		source: Arc<std::io::Error>,
	},

	/// The server's WebTransport response carried a status outside the valid HTTP range.
	#[error("invalid status code")]
	InvalidStatusCode,

	/// Reconnecting gave up, usually after the backoff timeout expired. The string has the details.
	#[error("{0}")]
	Reconnect(String),

	/// The connection was stopped locally, by closing it or dropping the last handle.
	///
	/// Not a failure: it is what a caller asked for. Distinct from [`Self::Reconnect`]
	/// so a status watcher can tell an expected teardown from a connection that gave up.
	#[error("connection stopped")]
	Stopped,

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

	/// The HTTP status a server answered a connection attempt with, if it answered with one at all.
	///
	/// `None` covers everything else: a dial that never got a response, a QUIC handshake that
	/// failed, a URL we couldn't parse. Only a status the peer actually sent shows up here, and
	/// whether it invites another attempt is the caller's call (`408`, `429`, `502`, `503`, and
	/// `504` are the ones worth repeating). This deliberately does not try to say whether some
	/// *other* kind of failure is worth retrying; that's a guess, and a backoff budget bounds it
	/// instead.
	pub fn status(&self) -> Option<u16> {
		match self {
			// A race is only settled when both halves were answered, and answered with something not
			// worth repeating: one transport being refused says nothing about the other, so a `404`
			// over QUIC alongside a dead WebSocket is still just a failed dial.
			#[cfg(feature = "websocket")]
			Self::TransportRace { quic, websocket } => match (quic.status(), websocket.status()) {
				(Some(quic), Some(websocket)) if !status_retryable(quic) && !status_retryable(websocket) => Some(quic),
				_ => None,
			},

			#[cfg(feature = "quinn")]
			Self::Quinn(err) => err.status(),
			#[cfg(feature = "noq")]
			Self::Noq(err) => err.status(),
			#[cfg(feature = "quiche")]
			Self::Quiche(err) => err.status(),
			#[cfg(feature = "websocket")]
			Self::WebSocket(err) => err.status(),
			_ => None,
		}
	}
}

// The wrapped sources aren't `Clone`, so `#[from]` can't store them behind `Arc`
// directly. These hand-written conversions keep `?` ergonomic at the call sites.
impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Self::Io(Arc::new(err))
	}
}

// Flattened to its message so `tracing-subscriber` stays out of this crate's public API.
impl From<tracing_subscriber::filter::ParseError> for Error {
	fn from(err: tracing_subscriber::filter::ParseError) -> Self {
		Self::Directive(err.to_string())
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
