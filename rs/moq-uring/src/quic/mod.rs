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

	/// The PEM certificate chain being presented, for a caller that publishes
	/// its fingerprint. The key is deliberately not readable back out.
	pub fn cert(&self) -> &[u8] {
		&self.cert
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

/// The per-connection transport knobs, the same for either role.
///
/// Separate from the role configs so a caller that already has these
/// settings (a relay applying its `--quic-*` section, say) sets them once and
/// hands the same value to a dial and a listener.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Transport {
	/// Close the connection after this long without activity.
	pub idle_timeout: std::time::Duration,
	/// The most streams of each kind (bidirectional and unidirectional) a peer
	/// may have open at once. MoQ opens a stream per group, so busy endpoints
	/// want this high.
	pub max_streams: u64,
	/// Which congestion controller to run.
	pub congestion: Congestion,
	/// How often to send an ack-eliciting packet on an otherwise idle
	/// connection, or `None` (the default) to send none and let the idle
	/// timeout decide.
	pub keep_alive: Option<std::time::Duration>,
}

impl Default for Transport {
	fn default() -> Self {
		Self {
			idle_timeout: std::time::Duration::from_secs(10),
			max_streams: 1024,
			congestion: Congestion::default(),
			keep_alive: None,
		}
	}
}

/// The congestion control family a connection runs.
///
/// The default is [`Loss`](Self::Loss), which is quiche's own, so a caller
/// that sets nothing gets what this crate has always done. An application
/// carrying live media wants [`Delay`](Self::Delay) and should say so; the
/// relay does.
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
}

impl Congestion {
	/// quiche takes its controller by name, and its BBR is the v2 gcongestion
	/// port.
	fn name(self) -> &'static str {
		match self {
			Self::Loss => "cubic",
			Self::Delay => "bbr2_gcongestion",
		}
	}
}

/// Build the quiche config shared by both roles.
pub(crate) fn tls(
	alpn: &[String],
	identity: Option<&Identity>,
	trust: Trust,
	transport: &Transport,
) -> Result<quiche::Config, Error> {
	use boring::ssl::{SslContextBuilder, SslMethod};

	let mut builder = SslContextBuilder::new(SslMethod::tls()).map_err(|err| Error::Tls(err.to_string()))?;
	apply_trust(&mut builder, &trust)?;

	if let Some(identity) = identity {
		apply_identity(&mut builder, identity)?;
	}

	let mut config = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)?;
	let alpn: Vec<&[u8]> = alpn.iter().map(|p| p.as_bytes()).collect();
	config.set_application_protos(&alpn)?;
	config.set_max_idle_timeout(transport.idle_timeout.as_millis() as u64);
	config.set_max_recv_udp_payload_size(SEGMENT);
	config.set_max_send_udp_payload_size(SEGMENT);
	config.set_initial_max_data(16 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_local(4 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_remote(4 * 1024 * 1024);
	config.set_initial_max_stream_data_uni(4 * 1024 * 1024);
	config.set_initial_max_streams_bidi(transport.max_streams);
	config.set_initial_max_streams_uni(transport.max_streams);
	config.set_cc_algorithm_name(transport.congestion.name())?;
	config.enable_dgram(true, 64, 64);
	Ok(config)
}

/// Present `identity`, from the PEM it already holds.
///
/// The file-based boring setters would re-read the paths on every call, which
/// is the whole reason [`Identity`] carries bytes.
fn apply_identity(builder: &mut boring::ssl::SslContextBuilder, identity: &Identity) -> Result<(), Error> {
	use boring::pkey::PKey;
	use boring::x509::X509;

	let tls = |err: boring::error::ErrorStack| Error::Tls(err.to_string());
	let chain = X509::stack_from_pem(identity.cert()).map_err(tls)?;
	let (leaf, intermediates) = chain
		.split_first()
		.ok_or_else(|| Error::Tls("certificate chain holds no certificates".to_string()))?;
	builder.set_certificate(leaf).map_err(tls)?;
	for cert in intermediates {
		builder.add_extra_chain_cert(cert.clone()).map_err(tls)?;
	}

	let key = PKey::private_key_from_pem(&identity.key).map_err(tls)?;
	builder.set_private_key(&key).map_err(tls)?;
	Ok(())
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
			for cert in read_roots(root)? {
				builder
					.cert_store_mut()
					.add_cert(cert)
					.map_err(|err| Error::Tls(format!("{}: {err}", root.display())))?;
			}
		}
	} else {
		let mut store = X509StoreBuilder::new().map_err(|err| Error::Tls(err.to_string()))?;
		for root in &trust.roots {
			for cert in read_roots(root)? {
				store
					.add_cert(cert)
					.map_err(|err| Error::Tls(format!("{}: {err}", root.display())))?;
			}
		}
		builder.set_cert_store_builder(store);
	}
	builder.set_verify(trust.verify);
	Ok(())
}

/// Read every PEM certificate in one root file, naming it when it fails.
///
/// A root is routinely a bundle of several CAs, and taking only the first
/// would reject a peer chaining to any of the others while looking configured.
/// A file holding none is an error rather than a store that trusts nothing.
fn read_roots(path: &std::path::Path) -> Result<Vec<boring::x509::X509>, Error> {
	let pem = std::fs::read(path).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
	let certs =
		boring::x509::X509::stack_from_pem(&pem).map_err(|err| Error::Tls(format!("{}: {err}", path.display())))?;
	if certs.is_empty() {
		return Err(Error::Tls(format!("{}: no certificates", path.display())));
	}
	Ok(certs)
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
