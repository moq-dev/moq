use crate::client::ClientConfig;
use crate::quic::Resolved;
use crate::server::{ServerConfig, ServerId};
use crate::tls::{FingerprintVerifier, ServeCerts};
use std::net;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;

use web_transport_noq::noq;

pub use web_transport_noq;

/// Apply the resolved quic knobs to a noq transport config.
fn apply_transport(transport: &mut noq::TransportConfig, quic: Resolved) {
	transport.max_idle_timeout(Some(quic.idle_timeout.try_into().expect("idle timeout out of range")));
	transport.keep_alive_interval(quic.keep_alive);

	// noq enables MTU discovery by default; disable it unless asked.
	if !quic.mtu_discovery {
		transport.mtu_discovery_config(None);
	}

	let max_streams = noq::VarInt::from_u64(quic.max_streams).unwrap_or(noq::VarInt::MAX);
	transport.max_concurrent_bidi_streams(max_streams);
	transport.max_concurrent_uni_streams(max_streams);

	if let Some(gso) = quic.gso {
		transport.enable_segmentation_offload(gso);
	}
}

/// Errors specific to the noq QUIC backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("failed to bind UDP socket")]
	BindSocket(#[source] std::io::Error),

	#[error("failed to create QUIC endpoint")]
	CreateEndpoint(#[source] std::io::Error),

	#[error("no async runtime")]
	NoRuntime,

	#[error("failed to get local address")]
	LocalAddr(#[source] std::io::Error),

	#[error("failed to resolve bind address")]
	ResolveBind(#[source] std::io::Error),

	#[error("invalid DNS name")]
	InvalidDnsName,

	#[error("failed DNS lookup")]
	DnsLookup(#[source] std::io::Error),

	#[error("no DNS entries")]
	NoDnsEntries,

	#[error("failed to fetch fingerprint")]
	FetchFingerprint(#[source] reqwest::Error),

	#[error("fingerprint request failed")]
	FingerprintStatus(#[source] reqwest::Error),

	#[error("failed to read fingerprint")]
	ReadFingerprint(#[source] reqwest::Error),

	#[error("invalid fingerprint")]
	InvalidFingerprint(#[from] hex::FromHexError),

	#[error("url scheme must be 'https', 'moqt', or 'moql'")]
	InvalidScheme,

	#[error("unsupported URL scheme: {0}")]
	UnsupportedScheme(String),

	#[error("missing handshake data")]
	MissingHandshake,

	#[error("missing ALPN")]
	MissingAlpn,

	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::string::FromUtf8Error),

	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	#[error("missing server name for raw QUIC connection")]
	MissingServerName,

	#[error("failed to construct URL from server name")]
	BuildUrl(#[source] url::ParseError),

	#[error("quic_lb_nonce must be at least 4")]
	QuicLbNonceTooSmall,

	#[error("connection ID length ({0}) exceeds maximum of 20")]
	QuicLbCidTooLong(usize),

	#[error("failed to build client certificate verifier")]
	ClientVerifier(#[source] rustls::server::VerifierBuilderError),

	#[error(transparent)]
	NoInitialCipherSuite(#[from] noq::crypto::rustls::NoInitialCipherSuite),

	#[error(transparent)]
	Connect(#[from] noq::ConnectError),

	#[error(transparent)]
	Connection(#[from] noq::ConnectionError),

	#[error(transparent)]
	Client(#[from] web_transport_noq::ClientError),

	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	#[error(transparent)]
	Server(#[from] web_transport_noq::ServerError),

	#[error("failed to establish QUIC connection")]
	Establish(#[source] noq::ConnectionError),

	#[error("failed to receive WebTransport request")]
	RecvRequest(#[source] web_transport_noq::ServerError),

	#[error(transparent)]
	Tls(#[from] crate::tls::Error),
}

type Result<T> = std::result::Result<T, Error>;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct NoqClient {
	pub quic: noq::Endpoint,
	pub transport: Arc<noq::TransportConfig>,
	/// Whether an `http://` URL may bootstrap a pin (see [crate::tls::Client::allows_http_bootstrap]).
	pub http_bootstrap: bool,
	/// Optional TLS SNI / verification hostname override (from config).
	pub host_name: Option<String>,
}

impl NoqClient {
	pub fn new(config: &ClientConfig) -> Result<Self> {
		let socket = crate::bind::udp(config.bind).map_err(Error::BindSocket)?;

		let mut transport = noq::TransportConfig::default();
		transport.congestion_controller_factory(Arc::new(noq::congestion::Bbr3Config::default()));
		apply_transport(&mut transport, config.quic.resolve());
		let transport = Arc::new(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = noq::default_runtime().ok_or(Error::NoRuntime)?;
		let endpoint_config = noq::EndpointConfig::default();

		// Create the generic QUIC endpoint.
		let quic = noq::Endpoint::new(endpoint_config, None, socket, runtime).map_err(Error::CreateEndpoint)?;

		Ok(Self {
			quic,
			transport,
			http_bootstrap: config.tls.allows_http_bootstrap(),
			host_name: config.tls.host_name.clone(),
		})
	}

	pub async fn connect(
		&self,
		tls: &rustls::ClientConfig,
		url: Url,
		versions: &moq_net::Versions,
	) -> Result<web_transport_noq::Session> {
		let mut url = url;
		let mut config = tls.clone();

		let host = url.host().ok_or(Error::InvalidDnsName)?.to_string();
		let port = url.port().unwrap_or(443);

		// Look up the DNS entry.
		// Noq doesn't support happy eyeballs, so we pick a single address,
		// preferring one whose family matches the local socket so the OS
		// doesn't reject it (notably on Windows, where IPv6 sockets aren't
		// dual-stack by default).
		let local = self.quic.local_addr().map_err(Error::LocalAddr)?;
		let addrs = tokio::net::lookup_host((host.clone(), port))
			.await
			.map_err(Error::DnsLookup)?;
		let ip = crate::util::pick_addr(addrs, local).ok_or(Error::NoDnsEntries)?;

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

				tracing::warn!(url = %fingerprint, "performing insecure HTTP request for certificate");

				let resp = reqwest::get(fingerprint.as_str())
					.await
					.map_err(Error::FetchFingerprint)?
					.error_for_status()
					.map_err(Error::FingerprintStatus)?;

				let fingerprint = resp.text().await.map_err(Error::ReadFingerprint)?;
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
			"https" => vec![web_transport_noq::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => versions.alpns().iter().map(|alpn| alpn.as_bytes().to_vec()).collect(),
			_ => return Err(Error::InvalidScheme),
		};

		config.alpn_protocols = alpns;
		config.key_log = Arc::new(rustls::KeyLogFile::new());

		let config: noq::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = noq::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(%url, %ip, "connecting");

		// Use the configured host_name override for SNI + cert verification, else the URL host.
		let host_name = self.host_name.clone().unwrap_or(host);

		let connection = self.quic.connect_with(config, ip, &host_name)?.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let mut request = web_transport_noq::proto::ConnectRequest::new(url.clone());
		for alpn in versions.alpns() {
			request = request.with_protocol(alpn.to_string());
		}

		let session = match url.scheme() {
			"https" => web_transport_noq::Session::connect(connection, request)
				.await
				.map_err(map_client_error)?,
			"moqt" | "moql" => {
				let handshake = connection
					.handshake_data()
					.ok_or(Error::MissingHandshake)?
					.downcast::<noq::crypto::rustls::HandshakeData>()
					.unwrap();

				let alpn = handshake.protocol.ok_or(Error::MissingAlpn)?;
				let alpn = String::from_utf8(alpn)?;

				let response = web_transport_noq::proto::ConnectResponse::OK.with_protocol(alpn);
				web_transport_noq::Session::raw(connection, request, response)
			}
			_ => return Err(Error::UnsupportedScheme(url.scheme().to_string())),
		};

		Ok(session)
	}
}

impl Error {
	pub(crate) fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::ConnectRejected(err) => Some(*err),
			Self::Client(err) => classify_client_error(err),
			_ => None,
		}
	}
}

fn map_client_error(err: web_transport_noq::ClientError) -> Error {
	if let Some(err) = classify_client_error(&err) {
		return err.into();
	}

	err.into()
}

fn classify_client_error(err: &web_transport_noq::ClientError) -> Option<crate::ConnectError> {
	match err {
		web_transport_noq::ClientError::HttpError(err) => classify_connect_error(err),
		_ => None,
	}
}

fn classify_connect_error(err: &web_transport_noq::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_noq::ConnectError::ErrorStatus(status) => crate::ConnectError::from_status_u16(status.as_u16()),
		web_transport_noq::ConnectError::ProtoError(err) => classify_proto_error(err),
		_ => None,
	}
}

fn classify_proto_error(err: &web_transport_noq::proto::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_noq::proto::ConnectError::ErrorStatus(status)
		| web_transport_noq::proto::ConnectError::WrongStatus(Some(status)) => {
			crate::ConnectError::from_status_u16(status.as_u16())
		}
		_ => None,
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct NoqServer {
	pub quic: noq::Endpoint,
	pub certs: Arc<ServeCerts>,
}

impl NoqServer {
	pub fn new(config: ServerConfig) -> Result<Self> {
		let mut transport = noq::TransportConfig::default();
		transport.congestion_controller_factory(Arc::new(noq::congestion::Bbr3Config::default()));
		apply_transport(&mut transport, config.quic.resolve());
		let transport = Arc::new(transport);

		let provider = crate::crypto::provider();

		let certs = ServeCerts::new(provider.clone());
		certs.load_certs(&config.tls)?;
		let certs = Arc::new(certs);

		let tls_builder = rustls::ServerConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.map_err(crate::tls::Error::from)?;

		let mut tls = if config.tls.root.is_empty() {
			tls_builder.with_no_client_auth().with_cert_resolver(certs.clone())
		} else {
			let roots = config.tls.load_roots()?;
			let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
				.allow_unauthenticated()
				.build()
				.map_err(Error::ClientVerifier)?;
			tls_builder
				.with_client_cert_verifier(verifier)
				.with_cert_resolver(certs.clone())
		};

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = config
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
		alpns.push(web_transport_noq::ALPN.as_bytes().to_vec());

		tls.alpn_protocols = alpns;
		tls.key_log = Arc::new(rustls::KeyLogFile::new());

		let tls: noq::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut tls = noq::ServerConfig::with_crypto(Arc::new(tls));
		tls.transport_config(transport);

		// Advertise the preferred_address transport parameter (RFC 9000 §9.6).
		// noq allocates a fresh CID + reset token for the address during the handshake.
		if let Some(addr) = config.quic.preferred_v4 {
			tls.preferred_address_v4(Some(addr));
		}
		if let Some(addr) = config.quic.preferred_v6 {
			tls.preferred_address_v6(Some(addr));
		}

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = noq::default_runtime().ok_or(Error::NoRuntime)?;

		let listen =
			crate::util::resolve(config.bind.as_deref(), crate::server::DEFAULT_BIND).map_err(Error::ResolveBind)?;

		// Configure connection ID generator with server ID if provided
		let mut endpoint_config = noq::EndpointConfig::default();
		if let Some(server_id) = config.quic.quic_lb_id {
			let nonce_len = config.quic.quic_lb_nonce.unwrap_or(8);
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

		let socket = crate::bind::udp(listen).map_err(Error::BindSocket)?;

		// Create the generic QUIC endpoint.
		let quic = noq::Endpoint::new(endpoint_config, Some(tls), socket, runtime).map_err(Error::CreateEndpoint)?;

		// Spawn the cert reload watcher only after endpoint creation succeeds,
		// so we don't leave a dangling watcher on failure.
		tokio::spawn(crate::tls::reload_certs(certs.clone(), config.tls.clone()));

		Ok(Self { quic, certs })
	}

	pub fn accept(&self) -> impl std::future::Future<Output = Option<noq::Incoming>> + '_ {
		self.quic.accept()
	}

	pub fn tls_info(&self) -> Arc<RwLock<crate::tls::Info>> {
		self.certs.info.clone()
	}

	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		self.quic.local_addr().map_err(Error::LocalAddr)
	}

	pub fn close(&self) {
		self.quic.close(noq::VarInt::from_u32(0), b"server shutdown");
	}
}

// ── NoqRequest ──────────────────────────────────────────────────────

/// A raw QUIC connection request without WebTransport framing (noq backend).
/// Accept a QUIC connection, negotiate WebTransport or raw moq, and complete the
/// handshake (a `200 OK` for WebTransport). Returns the established session plus the
/// request URL and validated mTLS identity, both captured before the response consumes
/// the request. Raw QUIC carries no request URL (the path rides the SETUP instead).
pub(crate) async fn accept(
	conn: noq::Incoming,
	alpns: Vec<&'static str>,
) -> Result<(
	web_transport_noq::Session,
	Option<Url>,
	Option<crate::tls::PeerIdentity>,
)> {
	let mut conn = conn.accept()?;

	let handshake = conn
		.handshake_data()
		.await?
		.downcast::<noq::crypto::rustls::HandshakeData>()
		.unwrap();

	let alpn = handshake.protocol.ok_or(Error::MissingAlpn)?;
	let alpn = String::from_utf8(alpn)?;
	let host = handshake.server_name.unwrap_or_default();

	// The established Connection no longer exposes a single peer address (noq 1.0
	// supports multipath), so capture it from the Connecting before awaiting.
	let remote = conn.remote_address();
	tracing::debug!(%host, ip = %remote, %alpn, "accepting");

	// Wait for the QUIC connection to be established.
	let conn = conn.await.map_err(Error::Establish)?;

	let span = tracing::Span::current();
	span.record("id", conn.stable_id());
	tracing::debug!(%host, ip = %remote, %alpn, "accepted");

	match alpn.as_str() {
		web_transport_noq::ALPN => {
			// Wait for the CONNECT request, then capture its URL and mTLS identity before
			// the response consumes it.
			let request = web_transport_noq::Request::accept(conn)
				.await
				.map_err(Error::RecvRequest)?;
			let url = Some(request.url.clone());
			let identity = crate::tls::PeerIdentity::from_any(request.conn().peer_identity());

			let mut response = web_transport_noq::proto::ConnectResponse::OK;
			if let Some(protocol) = request.protocols.iter().find(|p| alpns.contains(&p.as_str())) {
				response = response.with_protocol(protocol);
			}
			let session = request.respond(response).await.map_err(Error::Server)?;
			Ok((session, url, identity))
		}
		// Recognize any moq ALPN this server actually offered (its configured versions),
		// not the global default set. rustls only negotiates an ALPN the server offered, so
		// this covers opt-in / work-in-progress versions (e.g. moq-lite-06-wip) that are
		// deliberately absent from `moq_net::ALPNS`.
		alpn if alpns.contains(&alpn) => {
			if host.is_empty() {
				return Err(Error::MissingServerName);
			}
			let host_str = if host.contains(':') {
				format!("[{}]", host)
			} else {
				host.clone()
			};
			let url = format!("moqt://{}", host_str).parse::<Url>().map_err(Error::BuildUrl)?;
			let request = web_transport_noq::proto::ConnectRequest::new(url);
			let response = web_transport_noq::proto::ConnectResponse::OK.with_protocol(alpn);
			let identity = crate::tls::PeerIdentity::from_any(conn.peer_identity());
			// Raw QUIC carries no request URL; the path rides the SETUP.
			let session = web_transport_noq::Session::raw(conn, request, response);
			Ok((session, None, identity))
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

impl noq::ConnectionIdGenerator for ServerIdGenerator {
	fn generate_cid(&mut self) -> noq::ConnectionId {
		use rand::RngExt;
		let cid_len = self.cid_len();
		let mut cid = Vec::with_capacity(cid_len);
		// First byte has "self-encoded length" of server ID + nonce
		cid.push((cid_len - 1) as u8);
		cid.extend(self.server_id.0.iter());
		cid.extend(rand::rng().random_iter::<u8>().take(self.nonce_len));
		noq::ConnectionId::new(cid.as_slice())
	}

	fn cid_len(&self) -> usize {
		1 + self.server_id.len() + self.nonce_len
	}

	fn cid_lifetime(&self) -> Option<Duration> {
		None
	}
}
