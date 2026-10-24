//! The QUIC backend, used for both WebTransport (`https://`) and raw QUIC (`moqt://`, `moql://`).

use super::{driver, web};
use crate::RedactedUrl;
use crate::connect;
use crate::listen;
use crate::quic::CongestionControl;
use crate::quic::Resolved;
use crate::quic::ServerId;
use crate::tls::FingerprintVerifier;
use std::net;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll, Waker, ready};
use std::time::Duration;

/// Attach a qlog stream writing into the configured directory, if any.
///
/// A [`driver::QlogStream`] is a shared handle: every connection on the endpoint writes into
/// it, tagged with its own qlog `group_id`, so this is one file per endpoint.
fn apply_qlog(transport: &mut driver::TransportConfig, quic: &Resolved, role: &str) -> Result<()> {
	// `Client::validate` already rejected a directory this build can't honor, so the
	// block below is only reached where capture actually works.
	let Some(dir) = quic.qlog_dir() else {
		return Ok(());
	};

	#[cfg(feature = "qlog")]
	{
		// The pid and sequence keep endpoints sharing a directory from clobbering each other.
		static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
		let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let path = dir.join(format!("moq-{role}-{}-{seq}.sqlog", std::process::id()));
		let file = std::fs::File::create(&path).map_err(Error::CreateQlog)?;

		// Deliberately unbuffered: qlog's streamer only flushes on `finish_log`, which is
		// never called per event, so a BufWriter would hold every trace in memory until the
		// endpoint drops and lose the lot if the process is killed. Killing a stuck process
		// is exactly when these traces are worth having.
		let mut config = driver::QlogConfig::default();
		config.writer(Box::new(file)).title(Some(format!("moq-tokio {role}")));

		transport.qlog_stream(config.into_stream());
		tracing::info!(path = %path.display(), "writing qlog");
	}

	#[cfg(not(feature = "qlog"))]
	let _ = (transport, dir, role);

	Ok(())
}

/// Apply the resolved quic knobs to a transport config.
fn apply_transport(transport: &mut driver::TransportConfig, quic: &Resolved) {
	transport.max_idle_timeout(Some(quic.idle_timeout.try_into().expect("idle timeout out of range")));
	transport.keep_alive_interval(quic.keep_alive);

	// MTU discovery is enabled by default; disable it unless asked.
	if !quic.mtu_discovery {
		transport.mtu_discovery_config(None);
	}

	let max_streams = driver::VarInt::from_u64(quic.max_streams).unwrap_or(driver::VarInt::MAX);
	transport.max_concurrent_bidi_streams(max_streams);
	transport.max_concurrent_uni_streams(max_streams);

	if let Some(gso) = quic.gso {
		transport.enable_segmentation_offload(gso);
	}

	apply_windows(transport, quic);

	transport.congestion_controller_factory(congestion_factory(quic.congestion()));
}

/// Apply the flow-control windows `Config::resolve` settled, replacing moq-quic's own.
fn apply_windows(transport: &mut driver::TransportConfig, quic: &Resolved) {
	// Saturating rather than erroring: `Config::validate` already rejects a window
	// past the varint, so this only bites a `Resolved` built without it.
	transport.receive_window(driver::VarInt::from_u64(quic.receive_window).unwrap_or(driver::VarInt::MAX));
	transport
		.stream_receive_window(driver::VarInt::from_u64(quic.stream_receive_window).unwrap_or(driver::VarInt::MAX));
	transport.send_window(quic.send_window);
}

/// The controller factory for a congestion control family. moq-quic's BBR is v3.
fn congestion_factory(family: CongestionControl) -> Arc<dyn driver::congestion::ControllerFactory + Send + Sync> {
	match family {
		CongestionControl::Loss => Arc::new(driver::congestion::CubicConfig::default()),
		CongestionControl::Delay => Arc::new(driver::congestion::Bbr3Config::default()),
		CongestionControl::None => Arc::new(driver::congestion::Unlimited),
	}
}

/// A live QUIC connection under a session, for tuning it once the peer is known.
pub struct Connection(driver::Connection);

impl Connection {
	pub(crate) fn new(conn: driver::Connection) -> Self {
		Self(conn)
	}

	/// Change the flow-control limits `limits` sets; a raise reaches the peer on the next packet.
	///
	/// Errors, changing nothing, on limits [`Limits::validate`](crate::quic::Limits::validate) refuses.
	pub fn set_limits(&self, limits: crate::quic::Limits) -> Result<()> {
		Ok(self.0.set_limits(limits)?)
	}
}

/// Errors specific to the QUIC backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The UDP socket failed to bind, usually because the address is in use.
	#[error("failed to bind UDP socket")]
	BindSocket(#[source] std::io::Error),

	/// The QUIC endpoint could not be created around the bound socket.
	#[error("failed to create QUIC endpoint")]
	CreateEndpoint(#[source] std::io::Error),

	/// The qlog trace file couldn't be created, usually a missing directory.
	#[cfg(feature = "qlog")]
	#[error("failed to create qlog file")]
	CreateQlog(#[source] std::io::Error),

	/// No async runtime was found. Construct the client or server from within a tokio runtime.
	#[error("no async runtime")]
	NoRuntime,

	/// The endpoint's local address could not be read from the socket.
	#[error("failed to get local address")]
	LocalAddr(#[source] std::io::Error),

	/// The configured bind address could not be resolved.
	#[error("failed to resolve bind address")]
	ResolveBind(#[source] std::io::Error),

	/// The URL has no host to connect to.
	#[error("invalid DNS name")]
	InvalidDnsName,

	/// The URL host could not be resolved.
	#[error("failed DNS lookup")]
	DnsLookup(#[source] std::io::Error),

	/// DNS resolved the host to no addresses at all.
	#[error("no DNS entries")]
	NoDnsEntries,

	/// The insecure `http://` fingerprint fetch failed to reach the server.
	#[error("failed to fetch fingerprint: {0}")]
	FetchFingerprint(String),

	/// The server returned a non-success status for the fingerprint request.
	#[error("fingerprint request failed with status {0}")]
	FingerprintStatus(u16),

	/// The fingerprint response body could not be read.
	#[error("failed to read fingerprint: {0}")]
	ReadFingerprint(String),

	/// The fetched fingerprint was not valid hex.
	#[error("invalid fingerprint: {0}")]
	InvalidFingerprint(String),

	/// The URL scheme is not one this backend can dial.
	#[error("url scheme must be 'https', 'moqt', or 'moql'")]
	InvalidScheme,

	/// The URL scheme survived ALPN selection but has no session type.
	#[error("unsupported URL scheme: {0}")]
	UnsupportedScheme(String),

	/// The connection came up without TLS handshake data, so the ALPN is unknown.
	#[error("missing handshake data")]
	MissingHandshake,

	/// The peer negotiated no ALPN, so there's no protocol to speak.
	#[error("missing ALPN")]
	MissingAlpn,

	/// The negotiated ALPN was not valid UTF-8.
	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::string::FromUtf8Error),

	/// The peer negotiated an ALPN this endpoint doesn't serve.
	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	/// A raw QUIC client connected without SNI, so there's no host to build a URL from.
	#[error("missing server name for raw QUIC connection")]
	MissingServerName,

	/// The client's SNI host could not be parsed as a URL.
	#[error("failed to construct URL from server name: {0}")]
	BuildUrl(String),

	/// The configured QUIC-LB nonce is too short to be unique.
	#[error("quic_lb_nonce must be at least 4")]
	QuicLbNonceTooSmall,

	/// The server ID plus nonce don't fit in a QUIC connection ID. Shorten either.
	#[error("connection ID length ({0}) exceeds maximum of 20")]
	QuicLbCidTooLong(usize),

	/// Both QUIC-LB and reuseport steering want to own the connection ID, and
	/// each reads back only its own encoding.
	#[error("QUIC-LB connection IDs cannot be combined with per-core workers")]
	ShardWithQuicLb,

	/// The stateless reset secret file could not be read or does not hold 64 hexadecimal characters.
	#[error("invalid stateless reset secret")]
	ResetSecret(#[source] crate::secret::Error),

	/// The mTLS client verifier could not be built from the configured roots.
	#[error("failed to build client certificate verifier")]
	ClientVerifier(#[source] rustls::server::VerifierBuilderError),

	/// The TLS config lacks a cipher suite QUIC can use for initial packets.
	#[error("no cipher suite available for QUIC initial packets")]
	NoInitialCipherSuite,

	/// The connection could not be started, usually a bad config or address.
	#[error("{0}")]
	Connect(String),

	/// The QUIC connection failed or was closed.
	#[error("{0}")]
	Connection(String),

	/// The WebTransport CONNECT request failed.
	#[error("{message}")]
	Client {
		/// What the handshake reported.
		message: String,
		/// The HTTP status the server answered the CONNECT with, when it answered with one.
		///
		/// Read at conversion time rather than kept as a [`web`] error, so the
		/// classification survives without that crate appearing in this crate's public API.
		status: Option<u16>,
	},

	/// The server rejected the connection with a status we understand (auth, not found, etc).
	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	/// The WebTransport server failed to respond to a request.
	#[error("{0}")]
	Server(String),

	/// The QUIC handshake never completed for an incoming connection.
	#[error("failed to establish QUIC connection: {0}")]
	Establish(String),

	/// The client connected but never sent a valid WebTransport CONNECT request.
	#[error("failed to receive WebTransport request: {0}")]
	RecvRequest(String),

	/// The certificates or roots could not be loaded.
	#[error(transparent)]
	Tls(#[from] crate::tls::Error),

	/// Two or more addresses were raced and every attempt failed, each paired
	/// with its own error in dial order. All of them are kept: picking one to
	/// report would bury a rejected certificate or a refused port behind
	/// whichever address happened to be unroutable or to blackhole until its
	/// timeout. A host with a single address reports that error directly instead.
	#[error("all {} connection attempts failed: {}", .0.len(), crate::failover::describe(.0))]
	Failover(Vec<crate::failover::Attempt<Error>>),

	/// The flow-control limits are ones a peer would refuse, so none were applied.
	#[error("invalid quic limits: {0}")]
	Limits(#[from] moq_quic::LimitsError),
}

impl crate::failover::Aggregate for Error {
	fn aggregate(failures: Vec<crate::failover::Attempt<Self>>) -> Self {
		Self::Failover(failures)
	}

	fn resolve(error: Option<std::io::Error>) -> Self {
		match error {
			Some(error) => Self::DnsLookup(error),
			None => Self::NoDnsEntries,
		}
	}
}

crate::error::from_message! {
	driver::ConnectError => Connect,
	driver::ConnectionError => Connection,
	web::ServerError => Server,
	hex::FromHexError => InvalidFingerprint,
}

impl From<driver::crypto::rustls::NoInitialCipherSuite> for Error {
	fn from(_: driver::crypto::rustls::NoInitialCipherSuite) -> Self {
		Self::NoInitialCipherSuite
	}
}

impl From<web::ClientError> for Error {
	fn from(err: web::ClientError) -> Self {
		Self::Client {
			status: client_status(&err),
			message: crate::error::message(err),
		}
	}
}

type Result<T> = std::result::Result<T, Error>;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct Client {
	pub quic: driver::Endpoint,
	pub transport: Arc<driver::TransportConfig>,
	/// Whether an `http://` URL may bootstrap a pin (see [crate::tls::Connect::allows_http_bootstrap]).
	pub http_bootstrap: bool,
	/// Optional TLS SNI / verification hostname override (from config).
	pub host_name: Option<String>,
	/// Stagger between Happy Eyeballs connection attempts (see [`crate::failover`]).
	pub failover_delay: Duration,
	/// How long the first candidate waits for the full DNS answer, RFC 8305's
	/// Resolution Delay (see [`crate::connect::Config::resolution_delay`]).
	pub resolution_delay: Duration,
	/// Whether the bound socket really came back dual-stack, which decides
	/// whether an IPv4 destination is reachable at all. Captured here because the
	/// endpoint owns the socket from here on and `local_addr` can't tell us.
	dual_stack: bool,
	/// Whether to write session secrets to `SSLKEYLOGFILE` (see [`crate::quic::Config::keylog`]).
	keylog: bool,
}

impl Client {
	pub fn new(config: &connect::Config, quic: &crate::quic::Config) -> Result<Self> {
		let resolved = config.resolve();
		let socket = crate::bind::udp(crate::bind::Udp::new(resolved.bind)).map_err(Error::BindSocket)?;
		let dual_stack = crate::bind::udp_is_dual_stack(&socket);

		let mut transport = driver::TransportConfig::default();
		let quic = quic.resolve();
		apply_transport(&mut transport, &quic);
		apply_qlog(&mut transport, &quic, "client")?;
		let transport = Arc::new(transport);
		let keylog = quic.keylog;

		// There's a bit more boilerplate to make a generic endpoint.
		// The driver spawns its tasks on the current tokio runtime.
		tokio::runtime::Handle::try_current().map_err(|_| Error::NoRuntime)?;
		let endpoint_config = driver::EndpointConfig::default();

		// Create the generic QUIC endpoint.
		let quic = driver::Endpoint::new(endpoint_config, None, socket).map_err(Error::CreateEndpoint)?;

		Ok(Self {
			quic,
			transport,
			http_bootstrap: config.tls.allows_http_bootstrap(),
			host_name: config.tls.host_name.clone(),
			failover_delay: resolved.race,
			resolution_delay: resolved.resolution_delay,
			dual_stack,
			keylog,
		})
	}

	/// Close every connection, then wait until each has sent its close to the peer.
	pub async fn close(self) {
		self.quic.close(driver::VarInt::from_u32(0), b"client shutdown");
		// Not `wait_idle`, which also sits out each connection's 3 PTO closing
		// period: that only repeats the close to a peer that already has it.
		self.quic.wait_all_draining().await;
	}

	pub async fn connect(
		&self,
		tls: &rustls::ClientConfig,
		addr: crate::connect::Addr,
		versions: &moq_net::Versions,
	) -> Result<web::Session> {
		let mut url = addr.url().clone();
		let mut config = tls.clone();

		let target = url.host().ok_or(Error::InvalidDnsName)?;
		// URL brackets delimit IPv6 literals, but aren't part of the TLS server name.
		let host = match &target {
			url::Host::Ipv6(ip) => ip.to_string(),
			_ => target.to_string(),
		};
		let port = url.port().unwrap_or(443);

		// Use the configured host_name override for SNI + cert verification, else the URL host.
		let host_name = self.host_name.clone().unwrap_or(host);
		crate::tls::server_name(&host_name)?;

		// Resolve, adapted to the local socket's family; the dial below races the
		// answers Happy Eyeballs style as they land, so neither a broken family nor a
		// lookup still waiting on its AAAA record can stall the connect.
		let local = self.quic.local_addr().map_err(Error::LocalAddr)?;
		let candidates = match addr.addresses() {
			Some(addrs) => crate::resolve::Candidates::fixed(addrs.iter().copied()),
			None => crate::resolve::Candidates::resolve(target, port, self.resolution_delay),
		}
		.with_local(local, self.dual_stack);

		if url.scheme() == "http" {
			// Insecure per-connection bootstrap: only honored when no stronger
			// verification is configured, so an attacker controlling the plaintext
			// fetch can't weaken an explicit pin or re-enable disabled verification.
			if self.http_bootstrap {
				// Perform a HTTP request to fetch the certificate fingerprint.
				let mut fingerprint = url.clone();
				fingerprint.set_path("/certificate.sha256");
				fingerprint.set_query(None);
				fingerprint.set_fragment(None);

				tracing::warn!(url = %RedactedUrl::new(&fingerprint), "performing insecure HTTP request for certificate");

				// Even a plaintext fetch builds reqwest's TLS stack, which panics without a
				// process default when another crate (iroh) builds it as `rustls-no-provider`.
				// An error only means one is already installed.
				let _ = crate::crypto::install_default();

				let resp = reqwest::get(fingerprint.as_str())
					.await
					.map_err(|err| Error::FetchFingerprint(crate::error::message(err)))?;
				let status = resp.status().as_u16();
				let resp = resp.error_for_status().map_err(|_| Error::FingerprintStatus(status))?;

				let fingerprint = resp
					.text()
					.await
					.map_err(|err| Error::ReadFingerprint(crate::error::message(err)))?;
				let fingerprint = hex::decode(fingerprint.trim())?;

				let verifier = FingerprintVerifier::new(config.crypto_provider().clone(), vec![fingerprint]);
				config.dangerous().set_certificate_verifier(Arc::new(verifier));
			} else {
				tracing::warn!(
					"ignoring insecure http:// fingerprint bootstrap; using the configured TLS verification"
				);
			}

			url.set_scheme("https").expect("failed to set scheme");
		}

		let alpns: Vec<Vec<u8>> = match url.scheme() {
			"https" => vec![web::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => versions.alpns().iter().map(|alpn| alpn.as_bytes().to_vec()).collect(),
			_ => return Err(Error::InvalidScheme),
		};

		config.alpn_protocols = alpns;
		if self.keylog {
			config.key_log = Arc::new(rustls::KeyLogFile::new());
		}

		let config: driver::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = driver::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting");

		// Race only the QUIC handshake: the winner alone performs the WebTransport
		// CONNECT below, so the server sees a single request no matter how many
		// addresses were dialed.
		let connection = crate::failover::race(candidates, self.failover_delay, |addr| {
			let endpoint = self.quic.clone();
			let config = config.clone();
			let host_name = host_name.clone();
			async move { Ok::<_, Error>(endpoint.connect_with(config, addr, &host_name)?.await?) }
		})
		.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let session = match url.scheme() {
			"https" => {
				let mut request = web::proto::ConnectRequest::new(url.clone());
				for protocol in web_protocols(versions) {
					request = request.with_protocol(protocol.to_string());
				}
				web::Session::connect(connection, request)
					.await
					.map_err(map_client_error)?
			}
			"moqt" | "moql" => web::Session::raw(connection),
			_ => return Err(Error::UnsupportedScheme(url.scheme().to_string())),
		};

		Ok(session)
	}
}

impl Error {
	pub(crate) fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::ConnectRejected(err) => Some(*err),
			Self::Client {
				status: Some(status), ..
			} => crate::ConnectError::from_status_u16(*status),
			Self::Failover(failures) => failures.iter().find_map(|failure| failure.error.connect_error()),
			_ => None,
		}
	}

	/// A fixed local error or settled rejection that another dial cannot repair.
	pub(crate) fn is_terminal(&self) -> bool {
		match self {
			Self::InvalidScheme
			| Self::UnsupportedScheme(_)
			| Self::InvalidDnsName
			| Self::NoRuntime
			| Self::NoInitialCipherSuite
			| Self::ConnectRejected(_)
			| Self::Limits(_) => true,
			Self::Tls(err) => err.is_terminal(),
			Self::Failover(failures) => {
				!failures.is_empty() && failures.iter().all(|failure| failure.error.is_terminal())
			}
			_ => self
				.status()
				.is_some_and(|status| !crate::error::status_retryable(status)),
		}
	}

	/// The HTTP status a server answered with, if it answered with one at all.
	///
	/// Two places see a real status: the insecure `http://` fingerprint bootstrap, and the
	/// WebTransport CONNECT response. See [`crate::Error::status`].
	pub(crate) fn status(&self) -> Option<u16> {
		match self {
			Self::FingerprintStatus(status) => Some(*status),
			Self::Client { status, .. } => *status,
			// Every raced address has to have answered, and answered with something not worth
			// repeating, before the set counts as settled: one address refusing says nothing about
			// the others, which may simply have been unroutable.
			Self::Failover(failures) => {
				let mut settled = None;
				for failure in failures {
					match failure.error.status() {
						Some(status) if !crate::error::status_retryable(status) => settled = Some(status),
						_ => return None,
					}
				}
				settled
			}
			_ => None,
		}
	}
}

/// The WebTransport subprotocols to offer for `versions`.
///
/// Draft-14 predates WebTransport subprotocols: `moq-00` is only its QUIC ALPN, and a peer that
/// accepts it over WebTransport picks draft-14 over a newer draft both support. It rides `moql`
/// and negotiates in SETUP instead, as `js/net` does.
fn web_protocols(versions: &moq_net::Versions) -> Vec<&'static str> {
	let mut protocols = Vec::new();
	for alpn in versions.alpns() {
		let protocol = if alpn == "moq-00" { "moql" } else { alpn };
		if !protocols.contains(&protocol) {
			protocols.push(protocol);
		}
	}
	protocols
}

fn map_client_error(err: web::ClientError) -> Error {
	match client_status(&err).and_then(crate::ConnectError::from_status_u16) {
		Some(rejected) => rejected.into(),
		None => err.into(),
	}
}

/// The HTTP status the server answered the WebTransport CONNECT with, when it answered with one at
/// all (as opposed to the connection failing underneath the request).
///
/// Both classifications read this: [`classify_client_error`] turns an auth status into a
/// [`crate::ConnectError`], and [`Error::status`] hands it to the caller, whose backoff consults
/// the status. A `404` or `405` is the server's settled answer, so retrying
/// it just burns the reconnect budget on a URL that will never work.
fn client_status(err: &web::ClientError) -> Option<u16> {
	match err {
		web::ClientError::HttpError(err) => connect_status(err),
		_ => None,
	}
}

fn connect_status(err: &web::ConnectError) -> Option<u16> {
	match err {
		web::ConnectError::ErrorStatus(status) => Some(status.as_u16()),
		web::ConnectError::ProtoError(err) => proto_status(err),
		_ => None,
	}
}

fn proto_status(err: &web::proto::ConnectError) -> Option<u16> {
	match err {
		web::proto::ConnectError::ErrorStatus(status) | web::proto::ConnectError::WrongStatus(Some(status)) => {
			Some(status.as_u16())
		}
		_ => None,
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct Server {
	pub quic: driver::Endpoint,
	tls: crate::tls::Serve,
	socket: Socket,
}

/// The server's UDP socket, which [`Server::shutdown`] closes even while the driver
/// still holds references to it.
///
/// The driver gives each connection a sender sharing the socket, and a connection lives
/// as long as anything holds its handle, so dropping the endpoint leaves the port
/// bound until every accepted session is gone too. Taking the socket out of this
/// slot closes it regardless.
#[derive(Clone, Debug)]
struct Socket {
	io: Arc<RwLock<Option<Arc<tokio::net::UdpSocket>>>>,
	state: Arc<moq_sock::udp::UdpSocketState>,
	local: net::SocketAddr,
	/// Wakes every sender blocked on a full send buffer from the socket's single
	/// write-readiness registration.
	blocked: Arc<Blocked>,
	/// When a send error was last logged, shared so a busy server logs one per interval.
	last_send_error: Arc<Mutex<Option<driver::Instant>>>,
}

impl Socket {
	fn new(socket: std::net::UdpSocket) -> std::io::Result<Self> {
		let state = moq_sock::udp::UdpSocketState::new((&socket).into())?;
		let local = socket.local_addr()?;
		let io = tokio::net::UdpSocket::from_std(socket)?;
		Ok(Self {
			io: Arc::new(RwLock::new(Some(Arc::new(io)))),
			state: Arc::new(state),
			local,
			blocked: Default::default(),
			last_send_error: Default::default(),
		})
	}

	/// Close the socket. A reuseport member stays in its group regardless: the
	/// group retains a handle of its own.
	fn release(&self) {
		self.io.write().unwrap().take();
	}
}

impl driver::AsyncUdpSocket for Socket {
	fn create_sender(&self) -> Pin<Box<dyn driver::UdpSender>> {
		Box::pin(self.clone())
	}

	fn poll_recv(
		&mut self,
		cx: &mut Context<'_>,
		bufs: &mut [std::io::IoSliceMut<'_>],
		meta: &mut [moq_sock::udp::RecvMeta],
	) -> Poll<std::io::Result<usize>> {
		let io = self.io.read().unwrap();
		// Released: nothing arrives again, and the endpoint stops through its close.
		let Some(io) = io.as_deref() else {
			return Poll::Pending;
		};
		// Only the endpoint driver receives, so the one readiness waker is enough.
		loop {
			ready!(io.poll_recv_ready(cx))?;
			match io.try_io(tokio::io::Interest::READABLE, || self.state.recv(io.into(), bufs, meta)) {
				Ok(res) => return Poll::Ready(Ok(res)),
				// `try_io` clears readiness only for `WouldBlock`; retrying any other error
				// would spin, since `poll_recv_ready` completes at once and the recv fails again.
				Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
				Err(err) => return Poll::Ready(Err(err)),
			}
		}
	}

	fn local_addr(&self) -> std::io::Result<net::SocketAddr> {
		Ok(self.local)
	}

	fn max_receive_segments(&self) -> usize {
		self.state.gro_segments()
	}

	fn may_fragment(&self) -> bool {
		self.state.may_fragment()
	}
}

impl driver::UdpSender for Socket {
	fn poll_send(
		self: Pin<&mut Self>,
		transmit: &moq_sock::udp::Transmit<'_>,
		cx: &mut Context<'_>,
	) -> Poll<std::io::Result<()>> {
		let io = self.io.read().unwrap();
		// Released: the datagram is lost, which UDP always allows.
		let Some(io) = io.as_deref() else {
			return Poll::Ready(Ok(()));
		};
		loop {
			match io.try_io(tokio::io::Interest::WRITABLE, || {
				self.state.try_send(io.into(), transmit)
			}) {
				Ok(()) => return Poll::Ready(Ok(())),
				Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
				Err(err) => {
					let mut last = self.last_send_error.lock().unwrap();
					return Poll::Ready(driver::send_failed(&mut last, err, transmit));
				}
			}
			// Every connection sends from its own task, but tokio keeps one waker per
			// direction, so register a fan-out to all of them instead of this task's.
			self.blocked.push(cx.waker());
			let waker = Waker::from(self.blocked.clone());
			ready!(io.poll_send_ready(&mut Context::from_waker(&waker)))?;
		}
	}

	fn max_transmit_segments(&self) -> usize {
		self.state.max_gso_segments()
	}
}

/// The tasks waiting for a [`Socket`] to become writable.
#[derive(Debug, Default)]
struct Blocked(Mutex<Vec<Waker>>);

impl Blocked {
	fn push(&self, waker: &Waker) {
		let mut wakers = self.0.lock().unwrap();
		if !wakers.iter().any(|w| w.will_wake(waker)) {
			wakers.push(waker.clone());
		}
	}
}

impl std::task::Wake for Blocked {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		for waker in std::mem::take(&mut *self.0.lock().unwrap()) {
			waker.wake();
		}
	}
}

pub(crate) struct Options {
	pub config: listen::Config,
	pub quic: crate::quic::Config,
	pub member: Option<listen::Socket>,
	pub tls: crate::tls::Serve,
}

impl Server {
	pub fn new(options: Options) -> Result<Self> {
		let Options {
			config,
			quic,
			member,
			tls: served,
		} = options;
		let mut transport = driver::TransportConfig::default();
		let quic = quic.resolve();
		apply_transport(&mut transport, &quic);
		apply_qlog(&mut transport, &quic, "server")?;
		let keylog = quic.keylog;
		let transport = Arc::new(transport);

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = config
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
		alpns.push(web::ALPN.as_bytes().to_vec());

		let mut tls = (*served.config(alpns)?).clone();
		if keylog {
			tls.key_log = Arc::new(rustls::KeyLogFile::new());
		}

		let tls: driver::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut tls = driver::ServerConfig::with_crypto(Arc::new(tls));
		tls.transport_config(transport);

		// Advertise the preferred_address transport parameter (RFC 9000 §9.6).
		// The driver allocates a fresh CID + reset token for the address during the handshake.
		if let Some(addr) = config.preferred_v4 {
			tls.preferred_address_v4(Some(addr));
		}
		if let Some(addr) = config.preferred_v6 {
			tls.preferred_address_v6(Some(addr));
		}

		// There's a bit more boilerplate to make a generic endpoint.
		// The driver spawns its tasks on the current tokio runtime.
		tokio::runtime::Handle::try_current().map_err(|_| Error::NoRuntime)?;

		let listen = config
			.bind
			.as_ref()
			.map(crate::listen::Bind::resolve)
			.transpose()
			.map_err(Error::ResolveBind)?
			.unwrap_or(crate::server::DEFAULT_BIND);
		let load_balancer = config.load_balancer();

		let mut endpoint_config = driver::EndpointConfig::default();
		// First, since it also keys the default connection ID generator. The ones
		// below validate a predecessor's IDs without it.
		if let Some(secret) = &config.reset.secret {
			let secret = crate::secret::Secret::read(secret).map_err(Error::ResetSecret)?;
			endpoint_config.reset_secret(secret.as_bytes());
		}

		// Configure connection ID generator with server ID if provided
		if let Some(shard) = member.as_ref().map(listen::Socket::shard) {
			if load_balancer.is_some() {
				return Err(Error::ShardWithQuicLb);
			}
			// The steering filter recognizes short headers by their fixed bit.
			endpoint_config.grease_quic_bit(false);
			tracing::debug!(
				index = shard.index(),
				count = shard.count(),
				"encoding the shard in connection IDs"
			);
			endpoint_config.cid_generator(Arc::new(move || {
				let mut cids = moq_quic::RandomConnectionIdGenerator::default();
				cids.set_shard(shard.index(), shard.count());
				Box::new(cids)
			}));
		} else if let Some(load_balancer) = load_balancer {
			let server_id = load_balancer.id;
			let nonce_len = load_balancer.nonce;
			if nonce_len < 4 {
				return Err(Error::QuicLbNonceTooSmall);
			}

			let cid_len = 1 + server_id.len() + nonce_len;
			if cid_len > 20 {
				return Err(Error::QuicLbCidTooLong(cid_len));
			}

			tracing::info!(
				?server_id,
				nonce_len,
				"using QUIC-LB compatible connection ID generation"
			);
			endpoint_config.cid_generator(Arc::new(move || {
				Box::new(ServerIdGenerator::new(server_id.clone(), nonce_len))
			}));
		}

		// A group socket was released only after every member bound and the
		// steering filter covered the final array.
		let socket = match member {
			Some(member) => member.into_inner(),
			None => crate::bind::udp(crate::bind::Udp::new(listen)).map_err(Error::BindSocket)?,
		};

		// Create the generic QUIC endpoint.
		let socket = Socket::new(socket).map_err(Error::CreateEndpoint)?;
		let quic = driver::Endpoint::new_with_abstract_socket(endpoint_config, Some(tls), Box::new(socket.clone()))
			.map_err(Error::CreateEndpoint)?;

		Ok(Self {
			quic,
			tls: served,
			socket,
		})
	}

	pub fn accept(&self) -> impl std::future::Future<Output = Option<driver::Incoming>> + '_ {
		self.quic.accept()
	}

	pub fn certificates(&self) -> crate::tls::Certificates {
		self.tls.certificates()
	}

	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		self.quic.local_addr().map_err(Error::LocalAddr)
	}

	pub fn close(&self) {
		self.quic.close(driver::VarInt::from_u32(0), b"server shutdown");
	}

	/// Close every connection, wait until each has sent its close to the peer,
	/// then release the socket.
	pub async fn shutdown(self) {
		self.closer().close().await;
		self.socket.release();
	}

	/// A handle that closes this server's connections from wherever its owner
	/// went, for a worker that has to flush them before its runtime is dropped.
	pub fn closer(&self) -> Closer {
		Closer(self.quic.clone())
	}
}

/// Closes a [`Server`]'s connections without owning it; see [`Server::closer`].
pub(crate) struct Closer(driver::Endpoint);

impl Closer {
	/// Close every connection and wait until each has sent its close to the peer.
	pub async fn close(self) {
		self.0.close(driver::VarInt::from_u32(0), b"server shutdown");
		// Not `wait_idle`, which also sits out each connection's 3 PTO closing
		// period: that only repeats the close to a peer that already has it.
		self.0.wait_all_draining().await;
	}
}

// ── QuicRequest ─────────────────────────────────────────────────────

/// A raw QUIC connection request without WebTransport framing (QUIC backend).
/// Accept a QUIC connection, negotiate WebTransport or raw moq, and complete the
/// handshake (a `200 OK` for WebTransport). Returns the established session, the request
/// URL and validated mTLS identity (both captured before the response consumes the
/// request), and the dialed authority (the CONNECT authority on WebTransport, the TLS
/// SNI on raw QUIC). Raw QUIC carries no request URL (the path rides the SETUP instead).
pub(crate) async fn accept(
	conn: driver::Incoming,
	alpns: Vec<&'static str>,
) -> Result<crate::server::Accepted<web::Session>> {
	let mut conn = conn.accept()?;

	// Scoped so the `!Send` boxed handshake data is gone before the next await.
	let (alpn, host) = {
		let handshake = conn
			.handshake_data()
			.await?
			.downcast::<driver::crypto::rustls::HandshakeData>()
			.unwrap();
		(handshake.protocol, handshake.server_name)
	};

	let alpn = alpn.ok_or(Error::MissingAlpn)?;
	let alpn = String::from_utf8(alpn)?;
	let host = host.unwrap_or_default();

	let remote = conn.remote_address();
	tracing::debug!(%host, ip = %remote, %alpn, "accepting");

	// Wait for the QUIC connection to be established.
	let conn = conn.await.map_err(|err| Error::Establish(crate::error::message(err)))?;

	let span = tracing::Span::current();
	span.record("id", conn.stable_id());
	tracing::debug!(%host, ip = %remote, %alpn, "accepted");

	let link = crate::server::Link {
		remote: Some(remote),
		local: None,
		server_name: (!host.is_empty()).then(|| host.clone()),
		alpn: None,
	};

	match alpn.as_str() {
		web::ALPN => {
			// Wait for the CONNECT request, then capture its URL and mTLS identity before
			// the response consumes it.
			let request = web::Request::accept(conn)
				.await
				.map_err(|err| Error::RecvRequest(crate::error::message(err)))?;
			let url = Some(request.url.clone());
			let identity = crate::tls::PeerIdentity::from_any(request.conn().peer_identity());
			// The authority the client put in its CONNECT URL.
			let authority = request.url.host_str().filter(|h| !h.is_empty()).map(str::to_owned);

			let mut response = web::proto::ConnectResponse::OK;
			let mut link = link;
			if let Some(protocol) = request.protocols.iter().find(|p| alpns.contains(&p.as_str())) {
				response = response.with_protocol(protocol);
				link.alpn = Some(protocol.clone());
			}
			let session = request
				.respond(response)
				.await
				.map_err(|err| Error::Server(crate::error::message(err)))?;
			Ok(crate::server::Accepted {
				session,
				url,
				identity,
				authority,
				link,
			})
		}
		// Recognize any moq ALPN this server actually offered (its configured versions),
		// not the global default set. rustls only negotiates an ALPN the server offered, so
		// this also covers versions omitted from `moq_net::ALPNS` but enabled
		// in this server's configuration.
		alpn if alpns.contains(&alpn) => {
			let identity = crate::tls::PeerIdentity::from_any(conn.peer_identity());
			// Raw QUIC carries no request URL; the path rides the SETUP. The TLS SNI is the
			// only authority the client can offer here, and it is optional.
			let authority = (!host.is_empty()).then_some(host);
			let session = web::Session::raw(conn);
			Ok(crate::server::Accepted {
				session,
				url: None,
				identity,
				authority,
				link: crate::server::Link {
					alpn: Some(alpn.to_string()),
					..link
				},
			})
		}
		_ => Err(Error::UnsupportedAlpn(alpn)),
	}
}

// ── ServerIdGenerator ───────────────────────────────────────────────

struct ServerIdGenerator {
	server_id: ServerId,
	nonce_len: usize,
}

impl ServerIdGenerator {
	fn new(server_id: ServerId, nonce_len: usize) -> Self {
		Self { server_id, nonce_len }
	}
}

impl driver::ConnectionIdGenerator for ServerIdGenerator {
	fn generate_cid(&mut self) -> driver::ConnectionId {
		use rand::RngExt;
		let cid_len = self.cid_len();
		let mut cid = Vec::with_capacity(cid_len);
		// First byte has "self-encoded length" of server ID + nonce
		cid.push((cid_len - 1) as u8);
		cid.extend(self.server_id.0.iter());
		cid.extend(rand::rng().random_iter::<u8>().take(self.nonce_len));
		driver::ConnectionId::new(cid.as_slice())
	}

	fn cid_len(&self) -> usize {
		1 + self.server_id.len() + self.nonce_len
	}

	fn cid_lifetime(&self) -> Option<Duration> {
		None
	}
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use super::*;
	use url::Url;

	#[test]
	fn web_protocols_never_offer_moq_00() {
		let all = moq_net::Versions::all();
		assert!(!web_protocols(&all).contains(&"moq-00"));
		assert_eq!(web_protocols(&all).len(), all.alpns().len() - 1);

		let draft14: moq_net::Version = "moq-transport-14".parse().unwrap();
		assert_eq!(web_protocols(&draft14.into()), ["moql"]);
	}

	#[tokio::test]
	async fn pinned_ipv6_connection() {
		pinned_connection("[::1]:0", None).await;
	}

	#[tokio::test]
	async fn pinned_ipv4_connection() {
		pinned_connection("127.0.0.1:0", None).await;
	}

	#[tokio::test]
	async fn pinned_ipv6_connection_with_host_override() {
		pinned_connection("[::1]:0", Some("localhost")).await;
	}

	/// The server's socket hands a send that finds no route back to the connection, as the
	/// client's does, rather than logging it as loss. Linux refuses a v4-mapped destination on an
	/// IPv6-only socket with `ENETUNREACH`, an unroutable family without touching the host's routes.
	#[cfg(target_os = "linux")]
	#[tokio::test]
	async fn server_socket_returns_unreachable_sends() {
		let socket = socket2::Socket::new(
			socket2::Domain::IPV6,
			socket2::Type::DGRAM,
			Some(socket2::Protocol::UDP),
		)
		.unwrap();
		socket.set_only_v6(true).unwrap();
		socket.set_nonblocking(true).unwrap();
		socket
			.bind(&net::SocketAddr::from((net::Ipv6Addr::LOCALHOST, 0)).into())
			.unwrap();
		let socket = Socket::new(socket.into()).unwrap();

		let transmit = moq_sock::udp::Transmit {
			destination: net::SocketAddr::new(net::Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), 1),
			ecn: None,
			contents: b"hello",
			segment_size: None,
			src_ip: None,
		};
		let mut sender = driver::AsyncUdpSocket::create_sender(&socket);
		let result = std::future::poll_fn(|cx| sender.as_mut().poll_send(&transmit, cx)).await;

		let err = result.expect_err("an unroutable send must reach the connection");
		assert_eq!(err.kind(), std::io::ErrorKind::NetworkUnreachable);
	}

	/// The certificate fetch goes out without a process-default crypto provider installed,
	/// even when reqwest is built with rustls but no provider (as iroh builds it).
	#[tokio::test]
	async fn http_dial_needs_no_installed_provider() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let port = listener.local_addr().unwrap().port();
		tokio::spawn(async move {
			let (mut socket, _) = listener.accept().await.unwrap();
			let mut buf = [0; 1024];
			let _ = socket.read(&mut buf).await;
			let _ = socket
				.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
				.await;
		});

		let config = connect::Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			..Default::default()
		};
		let tls = config.tls.build().expect("tls config");
		let client = Client::new(&config, &crate::quic::Config::default()).expect("client init");
		let url: Url = format!("http://127.0.0.1:{port}/").parse().unwrap();
		let versions = moq_net::Versions::default();
		let result = tokio::time::timeout(Duration::from_secs(5), client.connect(&tls, url.into(), &versions))
			.await
			.expect("dial timed out");
		assert!(
			matches!(result, Err(Error::FingerprintStatus(404))),
			"the fetch should reach the server"
		);
	}

	async fn pinned_connection(bind: &str, host_name: Option<&str>) {
		let quic = crate::quic::Config::default();
		let config = listen::Config {
			bind: Some(bind.parse().unwrap()),
			tls: crate::tls::Listen {
				generate: vec!["localhost".into()],
				..Default::default()
			},
			..Default::default()
		};
		let tls = config.tls.load().unwrap();
		let server = Server::new(Options {
			config,
			quic: quic.clone(),
			member: None,
			tls,
		})
		.expect("server init");
		let addr = server.local_addr().expect("local addr");
		let mut tls_config = crate::tls::Connect::default();
		tls_config.fingerprint = server.certificates().fingerprints();
		assert!(!tls_config.fingerprint.is_empty());
		tls_config.host_name = host_name.map(str::to_owned);
		let config = connect::Config {
			bind: Some(bind.parse().unwrap()),
			tls: tls_config,
			..Default::default()
		};
		let tls = config.tls.build().expect("tls config");
		let client = Client::new(&config, &quic).expect("client init");
		let url: Url = format!("moqt://{addr}/.cluster/test").parse().unwrap();
		let versions = moq_net::Versions::default();
		let dial = client.connect(&tls, url.into(), &versions);
		let accept = async {
			let incoming = server.accept().await.expect("incoming connection");
			super::accept(incoming, versions.alpns()).await
		};
		let result = tokio::time::timeout(Duration::from_secs(5), async { tokio::try_join!(dial, accept) })
			.await
			.expect("handshake timed out");
		let (_client, _server) = result.expect("pinned connection failed");
	}

	/// moq-quic exposes no getters for the flow-control windows, but its `Debug` prints
	/// them, which is enough to prove each one reached the transport config, and that
	/// unset windows get our defaults instead of moq-quic's.
	#[test]
	fn apply_windows_writes_each_field() {
		let quic = crate::quic::Config {
			receive_window: Some(128 << 20),
			stream_receive_window: Some(8 << 20),
			send_window: Some(32 << 20),
			..Default::default()
		};

		let mut transport = driver::TransportConfig::default();
		apply_windows(&mut transport, &quic.resolve());
		let applied = format!("{:?}", transport);

		for field in [
			format!("receive_window: {}", 128 << 20),
			format!("stream_receive_window: {}", 8 << 20),
			format!("send_window: {}", 32 << 20),
		] {
			assert!(applied.contains(&field), "{field} missing from {applied}");
		}

		let mut untouched = driver::TransportConfig::default();
		apply_windows(&mut untouched, &crate::quic::Config::default().resolve());
		let applied = format!("{:?}", untouched);
		for field in [
			format!("receive_window: {}", crate::quic::DEFAULT_RECEIVE_WINDOW),
			format!("stream_receive_window: {}", crate::quic::DEFAULT_STREAM_RECEIVE_WINDOW),
			format!("send_window: {}", crate::quic::DEFAULT_SEND_WINDOW),
		] {
			assert!(applied.contains(&field), "{field} missing from {applied}");
		}
	}

	/// Build a controller from each family's factory and downcast it to the
	/// concrete moq-quic implementation it must map to.
	#[test]
	fn congestion_factory_maps_each_family() {
		let now = moq_clock::Instant::now();
		let mtu = 1200;

		let loss = congestion_factory(CongestionControl::Loss).build(now, mtu);
		assert!(loss.into_any().downcast::<driver::congestion::Cubic>().is_ok());

		let delay = congestion_factory(CongestionControl::Delay).build(now, mtu);
		assert!(delay.into_any().downcast::<driver::congestion::Bbr3>().is_ok());

		let none = congestion_factory(CongestionControl::None).build(now, mtu);
		assert!(none.into_any().downcast::<driver::congestion::Unlimited>().is_ok());
	}

	/// Loopback regression test: with the knob unset, live QUIC connections must run
	/// BBRv3 on both ends.
	#[tokio::test]
	async fn default_reaches_the_live_connection() {
		let server_config = listen::Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			tls: crate::tls::Listen {
				generate: vec!["localhost".into()],
				..Default::default()
			},
			..Default::default()
		};

		// The knob left unset, the way a binary that never mentions it runs.
		let quic = crate::quic::Config::default();

		let tls = server_config.tls.load().unwrap();
		let server = Server::new(Options {
			config: server_config,
			quic: quic.clone(),
			member: None,
			tls,
		})
		.expect("server init");
		let addr = server.local_addr().expect("local addr");

		let accepted = tokio::spawn(async move {
			let incoming = server.accept().await.expect("no incoming connection");
			let conn = incoming.accept().expect("accept").await.expect("handshake");
			is_bbr3(&conn)
		});

		// tls::Connect has a private field, so it can't be built with a struct literal.
		let mut tls_config = crate::tls::Connect::default();
		tls_config.insecure = Some(true);

		let client_config = connect::Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			tls: tls_config,
			..Default::default()
		};

		let tls = client_config.tls.build().expect("tls config");
		let client = Client::new(&client_config, &quic).expect("client init");
		// Dial the loopback IP directly so the system resolver is never involved.
		let url: Url = format!("moqt://127.0.0.1:{}", addr.port()).parse().unwrap();

		// Bound the whole connect + accept + assert flow so a handshake
		// regression fails fast instead of stalling CI.
		tokio::time::timeout(Duration::from_secs(5), async move {
			let session = client
				.connect(&tls, url.into(), &moq_net::Versions::default())
				.await
				.expect("connect failed");

			// web::Session derefs to the driver connection.
			assert!(is_bbr3(&session), "client connection is not running BBRv3");
			// BBRv3 can probe, so moq-net gets a target to write.
			#[cfg(feature = "net")]
			assert!(session.probe().is_some(), "BBRv3 connection has no probe target");
			assert!(
				accepted.await.expect("server task panicked"),
				"server connection is not running BBRv3"
			);
		})
		.await
		.expect("test timed out");
	}

	/// Whether a live connection is running BBRv3.
	fn is_bbr3(conn: &driver::Connection) -> bool {
		conn.congestion_state()
			.into_any()
			.downcast::<driver::congestion::Bbr3>()
			.is_ok()
	}
}
