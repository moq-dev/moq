//! QUIC connections over the worker's UDP path, as a MoQ transport.
//!
//! [`client::connect`] and [`server::accept`] wrap a sans-IO QUIC stack
//! around a [`udp::Socket`](crate::udp::Socket): a spawned driver task
//! shuttles packets between the socket and the connection (GSO trains out,
//! GRO coalesces in), arms the worker's userspace timers from the
//! connection's timeout, and wakes stream waiters. The returned
//! [`Connection`] implements [`web_transport_trait::poll`], so
//! wrapping it with `crate::transport::Session::new` and
//! [`moq_net::transport::Address::Setup`], then `crate::transport::Local`, lets
//! `moq_net::Client::connect` / `Server::accept` run real MoQ sessions on the
//! worker; everything is `Rc`-shared and `!Send` by design.
//!
//! An [`Endpoint`] serves many connections on one socket, demuxed by
//! connection id; [`client::connect`] and [`server::accept`] are its
//! single-connection shorthands. Native peers speak raw QUIC (the ALPN
//! carries the application protocol); browsers negotiate `h3` and get the
//! [`web`] layer's HTTP/3 CONNECT handshake on top of the same adapter, with
//! [`web::Session`] as the one transport type covering both.
//!
//! moq-quic is the sans-IO QUIC stack underneath the worker.

pub mod client;
pub mod endpoint;
pub mod handshake;
#[cfg(feature = "qlog")]
pub mod qlog;
pub mod server;
pub mod transport;
pub mod web;

mod driver;

pub use driver::{Connection, recv, send};
pub use endpoint::Endpoint;
/// The flow-control limits a live connection can change; see [`Connection::set_limits`].
pub use moq_quic::{Limits, LimitsError};
/// Stream identity, and the varint every QUIC code is.
pub use moq_quic::{VarInt, stream};

/// A TLS certificate chain and the private key that signs for it, as PEM.
/// One value, so neither half can be configured alone.
///
/// The bytes are read once and held, not re-read per connection: a worker
/// group builds one of these before it spawns, so replacing the files on disk
/// afterwards cannot leave two workers serving different identities.
#[derive(Clone)]
pub struct Identity {
	cert: Vec<u8>,
	key: Vec<u8>,
}

impl Identity {
	/// Read the PEM chain at `cert` and the PEM key at `key`.
	pub fn open(cert: impl AsRef<std::path::Path>, key: impl AsRef<std::path::Path>) -> Result<Self, Error> {
		let read = |path: &std::path::Path| {
			std::fs::read(path).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))
		};
		Ok(Self {
			cert: read(cert.as_ref())?,
			key: read(key.as_ref())?,
		})
	}

	/// The same, from PEM already in hand.
	pub fn from_pem(cert: impl Into<Vec<u8>>, key: impl Into<Vec<u8>>) -> Self {
		Self {
			cert: cert.into(),
			key: key.into(),
		}
	}

	/// A TLS 1.3 server policy presenting this identity, speaking `alpn` in
	/// preference order, and checking clients as `client_auth` says.
	pub fn server(
		&self,
		alpn: &[impl AsRef<str>],
		client_auth: server::ClientAuth,
	) -> Result<std::sync::Arc<rustls::ServerConfig>, Error> {
		let alpn = alpn.iter().map(|proto| proto.as_ref().as_bytes().to_vec()).collect();
		driver::identity_server_config(self, alpn, &client_auth)
	}

	/// The PEM certificate chain being presented, for a caller that publishes
	/// its fingerprint. The key is deliberately not readable back out.
	pub fn cert(&self) -> &[u8] {
		&self.cert
	}

	/// The PEM private key, for the backend loading it into its TLS stack.
	pub(crate) fn key(&self) -> &[u8] {
		&self.key
	}
}

impl std::fmt::Debug for Identity {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Whatever else gets logged, the private key does not.
		f.debug_struct("Identity")
			.field("cert", &format_args!("{} PEM bytes", self.cert.len()))
			.finish_non_exhaustive()
	}
}

/// The congestion control family a connection runs.
///
/// CUBIC is used for loss-based control and BBRv3 for delay-based control.
///
/// The default is [`Loss`](Self::Loss), which is what moq-quic runs
/// unasked. An application carrying live media wants [`Delay`](Self::Delay)
/// and should say so; the relay does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Congestion {
	/// Loss-based: grows until it drops packets, so the send rate sawtooths.
	#[default]
	Loss,
	/// Delay-based: tracks the measured delivery rate and RTT instead of
	/// waiting for loss, which keeps queues short and the send rate steady
	/// enough for an encoder to track.
	Delay,
	/// No congestion control and no pacing, for tests and controlled links.
	None,
}

/// Why a connection could not be set up or has ended.
///
/// One error type for the whole module: it is also what every
/// [`web_transport_trait::poll`] operation on a [`Connection`] reports, which
/// is how the close reason reaches `moq_net::Error::from_transport`.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The application (ours or the peer's) closed the connection.
	#[error("application closed: code={code} reason={reason:?}")]
	App {
		/// The application close code (a MoQ session code here).
		code: u64,
		/// The UTF-8 lossy close reason.
		reason: String,
	},
	/// QUIC closed the connection with a transport-level code.
	#[error("transport closed: code={code} reason={reason:?}")]
	Transport {
		/// The QUIC transport error code.
		code: u64,
		/// The UTF-8 lossy close reason.
		reason: String,
	},
	/// The connection idled out or the handshake never completed.
	#[error("connection timed out")]
	TimedOut,
	/// The peer reset the stream with this code.
	#[error("stream reset: {0}")]
	Reset(u64),
	/// The peer told us to stop sending with this code.
	#[error("stream stopped: {0}")]
	Stop(u64),
	/// The connection window was stuck and this stream was discarded to free
	/// it. See [`recv::Stream::poll_lease`]. The peer hears of it once the
	/// stream is stopped, so call `stop` with your own code: dropping the
	/// stream sends code 0.
	#[error("stream evicted")]
	Evicted,
	/// A lease wanted more than the connection receive window can hold.
	#[error("want exceeds the receive window")]
	TooLarge,
	/// The TLS material a connection needs could not be loaded.
	#[error("tls error: {0}")]
	Tls(String),
	/// The socket died underneath the connection.
	#[error("socket error: {0}")]
	Io(String),
	/// The QUIC stack refused an operation. The backend's own message, since
	/// the two describe the same failures differently.
	#[error("quic error: {0}")]
	Quic(String),
	/// Accepting needs the server configuration the endpoint was built
	/// without.
	#[error("endpoint has no server configuration")]
	NotServer,
	/// The WebTransport (HTTP/3) layer failed: a broken handshake, or a
	/// stream that could not be framed.
	#[error("webtransport error: {0}")]
	Web(String),
	/// HTTP/3 failed with a code of its own, one that names no WebTransport
	/// error (`H3_NO_ERROR`, say). Neither trait accessor reports it, because
	/// it is not a code the peer's application chose.
	#[error("http/3 closed: code={code} reason={reason:?}")]
	Http3 {
		/// The HTTP/3 error code.
		code: u64,
		/// The UTF-8 lossy close reason, empty for a stream-level code.
		reason: String,
	},
	/// qlog traces were asked for and cannot be captured: the directory is
	/// missing or unwritable, or the writer thread could not be started.
	#[error("qlog error: {0}")]
	Qlog(String),
	/// This endpoint closed the connection locally.
	#[error("locally closed")]
	Closed,
	/// The flow-control limits are ones a peer would refuse, so none were applied.
	#[error("invalid quic limits: {0}")]
	Limits(#[from] LimitsError),
	/// The transport settings are ones the QUIC stack refuses.
	#[error("invalid quic config: {0}")]
	Config(#[from] moq_quic::ConfigError),
}

impl web_transport_trait::Error for Error {
	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::App { code, reason } => Some((u32::try_from(*code).unwrap_or(u32::MAX), reason.clone())),
			_ => None,
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::Reset(code) | Self::Stop(code) => Some(u32::try_from(*code).unwrap_or(u32::MAX)),
			_ => None,
		}
	}
}
