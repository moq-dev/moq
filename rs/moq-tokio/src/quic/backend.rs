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
use std::sync::Arc;
use std::time::Duration;

/// The transport config the resolved quic knobs describe, for either role.
fn transport(quic: &Resolved, role: &str) -> Result<driver::transport::Config> {
	let mut transport = driver::transport::Config::default();
	transport.idle_timeout = Some(quic.idle_timeout);
	transport.keep_alive = quic.keep_alive;
	// MTU discovery is enabled by default; disable it unless asked.
	if !quic.mtu_discovery {
		transport.mtu = None;
	}
	// The windows replace moq-quic's own defaults, so every runtime honors the same section.
	transport.limits = quic.limits();
	if let Some(gso) = quic.gso {
		transport.segmentation_offload = gso;
	}
	transport.congestion = congestion_factory(quic.congestion());
	transport.validate()?;
	apply_qlog(&mut transport, quic, role)?;
	Ok(transport)
}

/// Attach a qlog stream writing into the configured directory, if any.
///
/// A [`driver::transport::qlog::Stream`] is a shared handle: every connection on the endpoint
/// writes into it, tagged with its own qlog `group_id`, so this is one file per endpoint.
fn apply_qlog(transport: &mut driver::transport::Config, quic: &Resolved, role: &str) -> Result<()> {
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
		let mut config = driver::transport::qlog::Config::default();
		config.writer = Some(Box::new(file));
		config.title = Some(format!("moq-tokio {role}"));

		transport.qlog = config.into_stream();
		tracing::info!(path = %path.display(), "writing qlog");
	}

	#[cfg(not(feature = "qlog"))]
	let _ = (transport, dir, role);

	Ok(())
}

/// The controller factory for a congestion control family. moq-quic's BBR is v3.
fn congestion_factory(family: CongestionControl) -> Arc<dyn moq_congestion::ControllerFactory + Send + Sync> {
	match family {
		CongestionControl::Loss => Arc::new(moq_congestion::CubicConfig::default()),
		CongestionControl::Delay => Arc::new(moq_congestion::Bbr3Config::default()),
		CongestionControl::None => Arc::new(moq_congestion::Unlimited),
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
	CreateEndpoint(#[source] driver::endpoint::Error),

	/// The resolved transport settings are ones the QUIC stack refuses.
	#[error("invalid quic config: {0}")]
	Config(#[from] driver::ConfigError),

	/// The qlog trace file couldn't be created, usually a missing directory.
	#[cfg(feature = "qlog")]
	#[error("failed to create qlog file")]
	CreateQlog(#[source] std::io::Error),

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
	driver::endpoint::ConnectError => Connect,
	driver::connection::Error => Connection,
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
	pub quic: driver::endpoint::Endpoint,
	pub transport: driver::transport::Config,
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
	/// A private log opened before this client can start a handshake.
	keylog: Option<Arc<crate::tls::KeyLog>>,
}

impl Client {
	pub fn new(config: &connect::Config, quic: &crate::quic::Config) -> Result<Self> {
		let resolved = config.resolve();
		let socket = crate::bind::udp(crate::bind::Udp::new(resolved.bind)).map_err(Error::BindSocket)?;
		let dual_stack = crate::bind::udp_is_dual_stack(&socket);

		let quic = quic.resolve();
		let transport = transport(&quic, "client")?;
		let keylog = quic
			.keylog
			.as_deref()
			.map(crate::tls::KeyLog::open)
			.transpose()?
			.map(Arc::new);

		let quic = driver::endpoint::Endpoint::new(driver::endpoint::Config::default(), socket)
			.map_err(Error::CreateEndpoint)?;

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
		self.quic.close(driver::VarInt::from_u32(0), "client shutdown").await;
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
		let local = self.quic.local_addr();
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
		if let Some(keylog) = &self.keylog {
			config.key_log = keylog.clone();
		}

		let config: driver::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = driver::client::Config::new(Arc::new(config));
		config.transport = self.transport.clone();

		tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting");

		// Race only the QUIC handshake: the winner alone performs the WebTransport
		// CONNECT below, so the server sees a single request no matter how many
		// addresses were dialed.
		let connection = crate::failover::race(candidates, self.failover_delay, |addr| {
			let connecting = self.quic.connect(&config, addr, &host_name);
			async move { Ok::<_, Error>(connecting?.await?) }
		})
		.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let session = match url.scheme() {
			"https" => {
				let mut request = web::proto::ConnectRequest::new(url.clone());
				for protocol in versions.alpns() {
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
			| Self::Config(_)
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
	pub quic: driver::endpoint::Endpoint,
	tls: crate::tls::Serve,
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
		let quic = quic.resolve();
		let transport = transport(&quic, "server")?;
		let keylog = quic
			.keylog
			.as_deref()
			.map(crate::tls::KeyLog::open)
			.transpose()?
			.map(Arc::new);

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = config
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
		alpns.push(web::ALPN.as_bytes().to_vec());

		let mut tls = (*served.config(alpns)?).clone();
		if let Some(keylog) = keylog {
			tls.key_log = keylog;
		}

		let tls: driver::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut server = driver::server::Config::new(Arc::new(tls));
		server.transport = transport;
		// Advertise the preferred_address transport parameter (RFC 9000 §9.6).
		// The driver allocates a fresh CID + reset token for the address during the handshake.
		server.preferred_address_v4 = config.preferred_v4;
		server.preferred_address_v6 = config.preferred_v6;

		let listen = config
			.bind
			.as_ref()
			.map(crate::listen::Bind::resolve)
			.transpose()
			.map_err(Error::ResolveBind)?
			.unwrap_or(crate::server::DEFAULT_BIND);
		let load_balancer = config.load_balancer();

		let mut endpoint_config = driver::endpoint::Config::default().with_server(server);
		// It also keys the default connection ID generator. The ones below validate a
		// predecessor's IDs without it.
		if let Some(secret) = &config.reset.secret {
			let secret = crate::secret::Secret::read(secret).map_err(Error::ResetSecret)?;
			endpoint_config.reset_secret = Some(*secret.as_bytes());
		}

		// Configure connection ID generator with server ID if provided
		if let Some(shard) = member.as_ref().map(listen::Socket::shard) {
			if load_balancer.is_some() {
				return Err(Error::ShardWithQuicLb);
			}
			// The steering filter recognizes short headers by their fixed bit.
			endpoint_config.grease_quic_bit = false;
			tracing::debug!(
				index = shard.index(),
				count = shard.count(),
				"encoding the shard in connection IDs"
			);
			endpoint_config.cid_generator = Some(Arc::new(move || {
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
			endpoint_config.cid_generator = Some(Arc::new(move || {
				Box::new(ServerIdGenerator::new(server_id.clone(), nonce_len))
			}));
		}

		// A group socket was released only after every member bound and the
		// steering filter covered the final array.
		let socket = match member {
			Some(member) => member.into_inner(),
			None => crate::bind::udp(crate::bind::Udp::new(listen)).map_err(Error::BindSocket)?,
		};

		let quic = driver::endpoint::Endpoint::new(endpoint_config, socket).map_err(Error::CreateEndpoint)?;

		Ok(Self { quic, tls: served })
	}

	pub fn accept(&self) -> impl std::future::Future<Output = Option<driver::endpoint::Incoming>> + '_ {
		self.quic.accept()
	}

	pub fn certificates(&self) -> crate::tls::Certificates {
		self.tls.certificates()
	}

	pub fn local_addr(&self) -> net::SocketAddr {
		self.quic.local_addr()
	}

	/// Close every connection without waiting for the closes to go out, as a drop must.
	pub fn close(&self) {
		drop(self.quic.close(driver::VarInt::from_u32(0), "server shutdown"));
	}

	/// Close every connection, wait until each has sent its close to the peer,
	/// then release the socket.
	pub async fn shutdown(self) {
		self.closer().close().await;
	}

	/// A handle that closes this server's connections from wherever its owner
	/// went, for a worker that has to flush them before its runtime is dropped.
	pub fn closer(&self) -> Closer {
		Closer(self.quic.clone())
	}
}

/// Closes a [`Server`]'s connections without owning it; see [`Server::closer`].
pub(crate) struct Closer(driver::endpoint::Endpoint);

impl Closer {
	/// Close every connection, wait until each has sent its close to the peer, then release
	/// the socket.
	pub async fn close(self) {
		self.0.close(driver::VarInt::from_u32(0), "server shutdown").await;
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
	conn: driver::endpoint::Incoming,
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
		let addr = server.local_addr();
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

	/// Every window reaches the transport config, and unset windows get our defaults instead
	/// of moq-quic's.
	#[test]
	fn transport_writes_each_window() {
		let quic = crate::quic::Config {
			receive_window: Some(128 << 20),
			stream_receive_window: Some(8 << 20),
			send_window: Some(32 << 20),
			..Default::default()
		};
		let limits = transport(&quic.resolve(), "client").unwrap().limits;
		assert_eq!(limits.receive_window.unwrap().get(), 128 << 20);
		assert_eq!(limits.stream_receive_window.unwrap().get(), 8 << 20);
		assert_eq!(limits.send_window.unwrap().get(), 32 << 20);

		let limits = transport(&crate::quic::Config::default().resolve(), "client")
			.unwrap()
			.limits;
		assert_eq!(
			limits.receive_window.unwrap().get(),
			crate::quic::DEFAULT_RECEIVE_WINDOW
		);
		assert_eq!(
			limits.stream_receive_window.unwrap().get(),
			crate::quic::DEFAULT_STREAM_RECEIVE_WINDOW
		);
		assert_eq!(limits.send_window.unwrap().get(), crate::quic::DEFAULT_SEND_WINDOW);
	}

	/// Build a controller from each family's factory and downcast it to the
	/// concrete moq-quic implementation it must map to.
	#[test]
	fn congestion_factory_maps_each_family() {
		let init = moq_congestion::Init {
			now: moq_clock::Instant::now(),
			mtu: 1200,
			rtt: std::time::Duration::from_millis(333),
			max_pacing_rate: None,
		};

		let loss = congestion_factory(CongestionControl::Loss).build(init);
		assert!(loss.into_any().downcast::<moq_congestion::Cubic>().is_ok());

		let delay = congestion_factory(CongestionControl::Delay).build(init);
		assert!(delay.into_any().downcast::<moq_congestion::Bbr3>().is_ok());

		let none = congestion_factory(CongestionControl::None).build(init);
		assert!(none.into_any().downcast::<moq_congestion::Unlimited>().is_ok());
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
		let addr = server.local_addr();

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
	fn is_bbr3(conn: &driver::connection::Connection) -> bool {
		conn.congestion_state()
			.into_any()
			.downcast::<moq_congestion::Bbr3>()
			.is_ok()
	}
}
