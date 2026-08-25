//! QUIC connections over the worker's UDP path, as a MoQ transport.
//!
//! [`connect`] and [`accept`] wrap sans-IO [`quiche`] around a
//! [`udp::Socket`]: a spawned driver task shuttles packets
//! between the socket and the connection (GSO trains out, GRO coalesces in),
//! arms the worker's userspace timers from quiche's timeout, and wakes stream
//! waiters. The returned [`Connection`] implements
//! [`web_transport_trait::poll`], so `moq_net::Client::connect_lite` /
//! `Server::accept_lite` run real moq-lite sessions on the worker; everything
//! is `Rc`-shared and `!Send` by design.
//!
//! This is raw QUIC: the ALPN carries the application protocol and there is no
//! HTTP/3 WebTransport layer (browsers need one; it can wrap this adapter
//! later). [`accept`] serves exactly one connection per socket; a multi
//! connection endpoint (connection-id demux, retry, version negotiation) comes
//! with the relay integration.

mod connection;
mod stream;

pub use connection::Connection;
pub use stream::{RecvStream, SendStream};

use std::net::SocketAddr;
use std::rc::Rc;

use crate::{Handle, udp};

/// The QUIC payload size every full datagram in a GSO train uses, and the
/// stride GRO coalesces with.
pub(crate) const SEGMENT: usize = 1350;

/// TLS and transport knobs for one connection.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// ALPN protocols to offer (client) or accept (server), in preference
	/// order. Required: QUIC without an application protocol is refused.
	pub alpn: Vec<String>,
	/// PEM certificate chain, required to accept.
	pub cert: Option<std::path::PathBuf>,
	/// PEM private key, required to accept.
	pub key: Option<std::path::PathBuf>,
	/// Verify the peer's certificate (default). Disable only for tests or
	/// pinned local deployments.
	pub verify_peer: bool,
	/// Close the connection after this long without activity.
	pub idle_timeout: std::time::Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			alpn: Vec::new(),
			cert: None,
			key: None,
			verify_peer: true,
			idle_timeout: std::time::Duration::from_secs(10),
		}
	}
}

impl Config {
	fn quiche(&self) -> Result<quiche::Config, Error> {
		let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;
		if let (Some(cert), Some(key)) = (&self.cert, &self.key) {
			config.load_cert_chain_from_pem_file(cert.to_str().ok_or(quiche::Error::TlsFail)?)?;
			config.load_priv_key_from_pem_file(key.to_str().ok_or(quiche::Error::TlsFail)?)?;
		}
		config.verify_peer(self.verify_peer);
		let alpn: Vec<&[u8]> = self.alpn.iter().map(|p| p.as_bytes()).collect();
		config.set_application_protos(&alpn)?;
		config.set_max_idle_timeout(self.idle_timeout.as_millis() as u64);
		config.set_max_recv_udp_payload_size(SEGMENT);
		config.set_max_send_udp_payload_size(SEGMENT);
		config.set_initial_max_data(16 * 1024 * 1024);
		config.set_initial_max_stream_data_bidi_local(4 * 1024 * 1024);
		config.set_initial_max_stream_data_bidi_remote(4 * 1024 * 1024);
		config.set_initial_max_stream_data_uni(4 * 1024 * 1024);
		config.set_initial_max_streams_bidi(256);
		config.set_initial_max_streams_uni(1024);
		config.enable_dgram(true, 64, 64);
		Ok(config)
	}
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
	/// The socket died underneath the connection.
	#[error("socket error: {0}")]
	Io(String),
	/// A quiche operation failed.
	#[error("quic error: {0}")]
	Quic(#[from] quiche::Error),
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

/// Dial `peer` over `socket`, driving the handshake to completion.
///
/// The connection's driver runs as a task on the worker behind `handle`, so
/// the returned [`Connection`] just works: hand it to
/// `moq_net::Client::connect_lite` or use the stream API directly.
pub async fn connect(
	handle: &Handle,
	socket: udp::Socket,
	server_name: &str,
	peer: SocketAddr,
	config: &Config,
) -> Result<Connection, Error> {
	let mut quiche_config = config.quiche()?;
	let local = socket.local_addr().map_err(|err| Error::Io(err.to_string()))?;
	let scid: [u8; quiche::MAX_CONN_ID_LEN] = rand::random();
	let scid = quiche::ConnectionId::from_ref(&scid);
	let conn = quiche::connect(Some(server_name), &scid, local, peer, &mut quiche_config)?;
	Connection::start(handle, socket, conn).await
}

/// Accept the one connection arriving on `socket`, driving the handshake to
/// completion.
///
/// The first packet decides the peer; anything else hitting the socket later
/// is fed to the same connection or dropped by quiche. One connection per
/// socket, by design: the multi-connection endpoint belongs to the relay
/// integration.
pub async fn accept(handle: &Handle, socket: udp::Socket, config: &Config) -> Result<Connection, Error> {
	let mut quiche_config = config.quiche()?;
	let local = socket.local_addr().map_err(|err| Error::Io(err.to_string()))?;

	// The peer announces itself with its Initial packet.
	let mut first = socket.recv().await.map_err(|err| Error::Io(err.to_string()))?;
	let peer = first.from();

	let scid: [u8; quiche::MAX_CONN_ID_LEN] = rand::random();
	let scid = quiche::ConnectionId::from_ref(&scid);
	let mut conn = quiche::accept(&scid, None, local, peer, &mut quiche_config)?;

	let info = quiche::RecvInfo { from: peer, to: local };
	for segment in first.segments() {
		// A malformed datagram is UDP noise, not fatal.
		if let Err(err) = conn.recv(segment, info) {
			tracing::debug!(%err, "quiche dropped a datagram");
		}
	}
	drop(first);

	Connection::start(handle, socket, conn).await
}

/// The state shared by every handle and the driver, single-threaded behind
/// `Rc<RefCell>`.
pub(crate) type Shared = Rc<connection::Inner>;
