//! QUIC connections over the worker's UDP path, as a MoQ transport.
//!
//! [`client::connect`] and [`server::accept`] wrap sans-IO [`quiche`] around a
//! [`udp::Socket`](crate::udp::Socket): a spawned driver task shuttles packets
//! between the socket and the connection (GSO trains out, GRO coalesces in),
//! arms the worker's userspace timers from quiche's timeout, and wakes stream
//! waiters. The returned [`Connection`] implements
//! [`web_transport_trait::poll`], so `moq_net::Client::connect_lite` /
//! `Server::accept_lite` run real moq-lite sessions on the worker; everything
//! is `Rc`-shared and `!Send` by design.
//!
//! An [`Endpoint`] serves many connections on one socket, demuxed by
//! connection id; [`client::connect`] and [`server::accept`] are its
//! single-connection shorthands. Native peers speak raw QUIC (the ALPN
//! carries the application protocol); browsers negotiate `h3` and get the
//! [`web`] layer's HTTP/3 CONNECT handshake on top of the same adapter, with
//! [`web::Session`] as the one transport type covering both.

pub mod client;
pub mod endpoint;
pub mod server;
pub mod web;

mod connection;
mod stream;

pub use connection::Connection;
pub use endpoint::Endpoint;
pub use stream::{RecvStream, SendStream};

pub(crate) use connection::Shared;

/// The QUIC payload size every full datagram in a GSO train uses, and the
/// stride GRO coalesces with.
pub(crate) const SEGMENT: usize = 1350;

/// The platform trust store, loaded when a role config asks for it.
const SYSTEM_ROOTS: &str = "/etc/ssl/certs";

/// A TLS certificate chain and the private key that signs for it, as PEM
/// files on disk. One value, so neither half can be configured alone.
#[derive(Clone, Debug)]
pub struct Identity {
	/// The PEM certificate chain to present.
	pub cert: std::path::PathBuf,
	/// The PEM private key for the leaf certificate.
	pub key: std::path::PathBuf,
}

impl Identity {
	/// The chain at `cert`, signed for by the key at `key`.
	pub fn new(cert: impl Into<std::path::PathBuf>, key: impl Into<std::path::PathBuf>) -> Self {
		Self {
			cert: cert.into(),
			key: key.into(),
		}
	}
}

/// Who to trust and how hard to insist, resolved from a role's config.
///
/// The store is built explicitly rather than added to, which is the whole
/// reason this goes through [`boring`] instead of [`quiche::Config::new`]:
/// that constructor loads the platform trust store before returning, so
/// "trust only these roots" cannot be expressed by omission.
pub(crate) struct Trust {
	/// Extra PEM root files to trust.
	pub roots: Vec<std::path::PathBuf>,
	/// Trust the platform store as well.
	pub system: bool,
	/// What to do about the peer's certificate.
	pub verify: boring::ssl::SslVerifyMode,
}

/// Build the quiche config shared by both roles.
pub(crate) fn tls(
	alpn: &[String],
	identity: Option<&Identity>,
	trust: Trust,
	idle_timeout: std::time::Duration,
) -> Result<quiche::Config, Error> {
	use boring::ssl::{SslContextBuilder, SslFiletype, SslMethod};

	let mut builder = SslContextBuilder::new(SslMethod::tls()).map_err(|err| Error::Tls(err.to_string()))?;
	apply_trust(&mut builder, &trust)?;

	if let Some(identity) = identity {
		builder
			.set_certificate_chain_file(&identity.cert)
			.map_err(|err| Error::Tls(format!("{}: {err}", identity.cert.display())))?;
		builder
			.set_private_key_file(&identity.key, SslFiletype::PEM)
			.map_err(|err| Error::Tls(format!("{}: {err}", identity.key.display())))?;
	}

	let mut config = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)?;
	let alpn: Vec<&[u8]> = alpn.iter().map(|p| p.as_bytes()).collect();
	config.set_application_protos(&alpn)?;
	config.set_max_idle_timeout(idle_timeout.as_millis() as u64);
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

/// Point `builder` at exactly the roots `trust` names.
///
/// With [`Trust::system`] off the store is built from scratch, which is the
/// only way to mean it: adding to the store quiche's own constructor leaves
/// behind would trust the platform's CAs on top of the configured ones.
fn apply_trust(builder: &mut boring::ssl::SslContextBuilder, trust: &Trust) -> Result<(), Error> {
	use boring::x509::store::X509StoreBuilder;

	if trust.system {
		builder
			.set_default_verify_paths()
			.map_err(|err| Error::Tls(format!("{SYSTEM_ROOTS}: {err}")))?;
		for root in &trust.roots {
			let cert = read_root(root)?;
			builder
				.cert_store_mut()
				.add_cert(cert)
				.map_err(|err| Error::Tls(format!("{}: {err}", root.display())))?;
		}
	} else {
		let mut store = X509StoreBuilder::new().map_err(|err| Error::Tls(err.to_string()))?;
		for root in &trust.roots {
			let cert = read_root(root)?;
			store
				.add_cert(cert)
				.map_err(|err| Error::Tls(format!("{}: {err}", root.display())))?;
		}
		builder.set_cert_store_builder(store);
	}
	builder.set_verify(trust.verify);
	Ok(())
}

/// Read one PEM root, naming the file when it fails.
fn read_root(path: &std::path::Path) -> Result<boring::x509::X509, Error> {
	let pem = std::fs::read(path).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
	boring::x509::X509::from_pem(&pem).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))
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
	/// The TLS material a connection needs could not be loaded.
	#[error("tls error: {0}")]
	Tls(String),
	/// The socket died underneath the connection.
	#[error("socket error: {0}")]
	Io(String),
	/// A quiche operation failed.
	#[error("quic error: {0}")]
	Quic(#[from] quiche::Error),
	/// Accepting needs the server configuration the endpoint was built
	/// without.
	#[error("endpoint has no server configuration")]
	NotServer,
	/// The WebTransport (HTTP/3) layer failed: a broken handshake, or a
	/// stream that could not be framed.
	#[error("webtransport error: {0}")]
	Web(String),
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
