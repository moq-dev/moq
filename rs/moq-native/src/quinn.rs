//! The quinn QUIC backend, used for both WebTransport (`https://`) and raw QUIC (`moqt://`, `moql://`).

use crate::client::ClientConfig;
use crate::quic::CongestionControl;
use crate::quic::Resolved;
use crate::quic::ServerId;
use crate::server::ServerConfig;
use crate::tls::{FingerprintVerifier, ServeCerts};
use std::net;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub use web_transport_quinn;

/// Apply the resolved quic knobs to a quinn transport config.
fn apply_transport(transport: &mut quinn::TransportConfig, quic: Resolved) {
	transport.max_idle_timeout(Some(quic.idle_timeout.try_into().expect("idle timeout out of range")));
	transport.keep_alive_interval(quic.keep_alive);

	// quinn enables MTU discovery by default; disable it unless asked.
	if !quic.mtu_discovery {
		transport.mtu_discovery_config(None);
	}

	let max_streams = quinn::VarInt::from_u64(quic.max_streams).unwrap_or(quinn::VarInt::MAX);
	transport.max_concurrent_bidi_streams(max_streams);
	transport.max_concurrent_uni_streams(max_streams);

	// GSO is on by default; only the quinn/noq backends can turn it off.
	if let Some(gso) = quic.gso {
		transport.enable_segmentation_offload(gso);
	}

	// quinn defaults to CUBIC, so an unset knob leaves the factory alone.
	if let Some(family) = quic.congestion_control {
		transport.congestion_controller_factory(congestion_factory(family));
	}
}

/// The quinn controller factory for a congestion control family. quinn's BBR is v1.
fn congestion_factory(family: CongestionControl) -> Arc<dyn quinn::congestion::ControllerFactory + Send + Sync> {
	match family {
		CongestionControl::Loss => Arc::new(quinn::congestion::CubicConfig::default()),
		CongestionControl::Delay => Arc::new(quinn::congestion::BbrConfig::default()),
	}
}

/// Errors specific to the quinn QUIC backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The UDP socket couldn't be bound, usually because the address is already in use.
	#[error("failed to bind UDP socket")]
	BindSocket(#[source] std::io::Error),

	/// The bound socket couldn't be turned into a QUIC endpoint.
	#[error("failed to create QUIC endpoint")]
	CreateEndpoint(#[source] std::io::Error),

	/// Quinn found no async runtime. Construct the client or server from within a tokio context.
	#[error("no async runtime")]
	NoRuntime,

	/// The endpoint's local address couldn't be read back from the OS.
	#[error("failed to get local address")]
	LocalAddr(#[source] std::io::Error),

	/// The server's configured bind address couldn't be resolved.
	#[error("failed to resolve bind address")]
	ResolveBind(#[source] std::io::Error),

	/// The URL has no host to connect to.
	#[error("invalid DNS name")]
	InvalidDnsName,

	/// Resolving the URL's host failed.
	#[error("failed DNS lookup")]
	DnsLookup(#[source] std::io::Error),

	/// DNS returned no address usable from the local socket, usually an address family mismatch.
	#[error("no DNS entries")]
	NoDnsEntries,

	/// The insecure `http://` bootstrap couldn't fetch `/certificate.sha256`.
	#[error("failed to fetch fingerprint")]
	FetchFingerprint(#[source] reqwest::Error),

	/// The `/certificate.sha256` fetch returned a non-success status.
	#[error("fingerprint request failed")]
	FingerprintStatus(#[source] reqwest::Error),

	/// The fingerprint response body couldn't be read.
	#[error("failed to read fingerprint")]
	ReadFingerprint(#[source] reqwest::Error),

	/// The fetched fingerprint wasn't valid hex.
	#[error("invalid fingerprint")]
	InvalidFingerprint(#[from] hex::FromHexError),

	/// The URL scheme isn't one this backend can dial.
	#[error("url scheme must be 'https', 'moqt', or 'moql'")]
	InvalidScheme,

	/// The URL scheme passed the initial check but has no session type, which means it slipped through a scheme list.
	#[error("unsupported URL scheme: {0}")]
	UnsupportedScheme(String),

	/// The connection came up without TLS handshake data, so the negotiated ALPN can't be read.
	#[error("missing handshake data")]
	MissingHandshake,

	/// TLS negotiated no ALPN, so there's no protocol to speak.
	#[error("missing ALPN")]
	MissingAlpn,

	/// The negotiated ALPN wasn't valid UTF-8.
	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::string::FromUtf8Error),

	/// The peer negotiated an ALPN this endpoint doesn't handle.
	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	/// A raw QUIC client connected without SNI, so the server can't tell which host it wanted.
	#[error("missing server name for raw QUIC connection")]
	MissingServerName,

	/// The client's SNI hostname didn't form a valid URL.
	#[error("failed to construct URL from server name")]
	BuildUrl(#[source] url::ParseError),

	/// The configured QUIC-LB nonce is too short to be unguessable.
	#[error("quic_lb_nonce must be at least 4")]
	QuicLbNonceTooSmall,

	/// The QUIC-LB server ID plus nonce doesn't fit in a connection ID. Shorten one of them.
	#[error("connection ID length ({0}) exceeds maximum of 20")]
	QuicLbCidTooLong(usize),

	/// The mTLS client verifier couldn't be built from the configured roots.
	#[error("failed to build client certificate verifier")]
	ClientVerifier(#[source] rustls::server::VerifierBuilderError),

	/// The rustls crypto provider offers no cipher suite usable for QUIC initial packets.
	#[error(transparent)]
	NoInitialCipherSuite(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

	/// Quinn refused to start the connection, before any packet was sent.
	#[error(transparent)]
	Connect(#[from] quinn::ConnectError),

	/// The QUIC connection failed or was closed by the peer.
	#[error(transparent)]
	Connection(#[from] quinn::ConnectionError),

	/// The WebTransport client handshake failed.
	#[error(transparent)]
	Client(#[from] web_transport_quinn::ClientError),

	/// The server answered the WebTransport CONNECT with a rejection status.
	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	/// The WebTransport server handshake failed while responding.
	#[error(transparent)]
	Server(#[from] web_transport_quinn::ServerError),

	/// The QUIC handshake didn't complete for an incoming connection.
	#[error("failed to establish QUIC connection")]
	Establish(#[source] quinn::ConnectionError),

	/// The client never sent a usable WebTransport CONNECT request.
	#[error("failed to receive WebTransport request")]
	RecvRequest(#[source] web_transport_quinn::ServerError),

	/// The TLS configuration or certificates couldn't be loaded.
	#[error(transparent)]
	Tls(#[from] crate::tls::Error),
}

type Result<T> = std::result::Result<T, Error>;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuinnClient {
	pub quic: quinn::Endpoint,
	pub transport: Arc<quinn::TransportConfig>,
	/// Whether an `http://` URL may bootstrap a pin (see [crate::tls::Client::allows_http_bootstrap]).
	pub http_bootstrap: bool,
	/// Optional TLS SNI / verification hostname override (from config).
	pub host_name: Option<String>,
}

impl QuinnClient {
	pub fn new(config: &ClientConfig) -> Result<Self> {
		let socket = crate::bind::udp(config.bind).map_err(Error::BindSocket)?;

		let mut transport = quinn::TransportConfig::default();
		apply_transport(&mut transport, config.quic.resolve());
		let transport = Arc::new(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().ok_or(Error::NoRuntime)?;
		let endpoint_config = quinn::EndpointConfig::default();

		// Create the generic QUIC endpoint.
		let quic = quinn::Endpoint::new(endpoint_config, None, socket, runtime).map_err(Error::CreateEndpoint)?;

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
	) -> Result<web_transport_quinn::Session> {
		let mut url = url;
		let mut config = tls.clone();

		let host = url.host().ok_or(Error::InvalidDnsName)?.to_string();
		let port = url.port().unwrap_or(443);

		// Look up the DNS entry.
		// Quinn doesn't support happy eyeballs, so we pick a single address,
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
			"https" => vec![web_transport_quinn::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => versions.alpns().iter().map(|alpn| alpn.as_bytes().to_vec()).collect(),
			_ => return Err(Error::InvalidScheme),
		};

		config.alpn_protocols = alpns;
		config.key_log = Arc::new(rustls::KeyLogFile::new());

		let config: quinn::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = quinn::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(%url, %ip, "connecting");

		// Use the configured host_name override for SNI + cert verification, else the URL host.
		let host_name = self.host_name.clone().unwrap_or(host);

		let connection = self.quic.connect_with(config, ip, &host_name)?.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let mut request = web_transport_quinn::proto::ConnectRequest::new(url.clone());
		for alpn in versions.alpns() {
			request = request.with_protocol(alpn.to_string());
		}

		let session = match url.scheme() {
			"https" => web_transport_quinn::Session::connect(connection, request)
				.await
				.map_err(map_client_error)?,
			"moqt" | "moql" => {
				let handshake = connection
					.handshake_data()
					.ok_or(Error::MissingHandshake)?
					.downcast::<quinn::crypto::rustls::HandshakeData>()
					.unwrap();

				let alpn = handshake.protocol.ok_or(Error::MissingAlpn)?;
				let alpn = String::from_utf8(alpn)?;

				let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(alpn);
				web_transport_quinn::Session::raw(connection, request, response)
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

fn map_client_error(err: web_transport_quinn::ClientError) -> Error {
	if let Some(err) = classify_client_error(&err) {
		return err.into();
	}

	err.into()
}

fn classify_client_error(err: &web_transport_quinn::ClientError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quinn::ClientError::HttpError(err) => classify_connect_error(err),
		_ => None,
	}
}

fn classify_connect_error(err: &web_transport_quinn::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quinn::ConnectError::ErrorStatus(status) => crate::ConnectError::from_status_u16(status.as_u16()),
		web_transport_quinn::ConnectError::ProtoError(err) => classify_proto_error(err),
		_ => None,
	}
}

fn classify_proto_error(err: &web_transport_quinn::proto::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quinn::proto::ConnectError::ErrorStatus(status)
		| web_transport_quinn::proto::ConnectError::WrongStatus(Some(status)) => {
			crate::ConnectError::from_status_u16(status.as_u16())
		}
		_ => None,
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct QuinnServer {
	pub quic: quinn::Endpoint,
	pub certs: Arc<ServeCerts>,
}

impl QuinnServer {
	pub fn new(config: ServerConfig) -> Result<Self> {
		let mut transport = quinn::TransportConfig::default();
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
		alpns.push(web_transport_quinn::ALPN.as_bytes().to_vec());

		tls.alpn_protocols = alpns;
		tls.key_log = Arc::new(rustls::KeyLogFile::new());

		let tls: quinn::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut tls = quinn::ServerConfig::with_crypto(Arc::new(tls));
		tls.transport_config(transport);

		// Advertise the preferred_address transport parameter (RFC 9000 §9.6).
		// Quinn allocates a fresh CID + reset token for the address during the handshake.
		if let Some(addr) = config.quic.preferred_v4 {
			tls.preferred_address_v4(Some(addr));
		}
		if let Some(addr) = config.quic.preferred_v6 {
			tls.preferred_address_v6(Some(addr));
		}

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().ok_or(Error::NoRuntime)?;

		let listen =
			crate::util::resolve(config.bind.as_deref(), crate::server::DEFAULT_BIND).map_err(Error::ResolveBind)?;

		// Configure connection ID generator with server ID if provided
		let mut endpoint_config = quinn::EndpointConfig::default();
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
			endpoint_config.cid_generator(move || Box::new(ServerIdGenerator::new(server_id.clone(), nonce_len)));
		}

		let socket = crate::bind::udp(listen).map_err(Error::BindSocket)?;

		// Create the generic QUIC endpoint.
		let quic = quinn::Endpoint::new(endpoint_config, Some(tls), socket, runtime).map_err(Error::CreateEndpoint)?;

		// Spawn the cert reload watcher only after endpoint creation succeeds,
		// so we don't leave a dangling watcher on failure.
		tokio::spawn(crate::tls::reload_certs(certs.clone(), config.tls.clone()));

		Ok(Self { quic, certs })
	}

	pub fn accept(&self) -> impl std::future::Future<Output = Option<quinn::Incoming>> + '_ {
		self.quic.accept()
	}

	pub fn certificates(&self) -> crate::tls::Certificates {
		crate::tls::Certificates::new(self.certs.info.clone())
	}

	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		self.quic.local_addr().map_err(Error::LocalAddr)
	}

	pub fn close(&self) {
		self.quic.close(quinn::VarInt::from_u32(0), b"server shutdown");
	}
}

// ── QuinnRequest ────────────────────────────────────────────────────

/// Accept a QUIC connection, negotiate WebTransport or raw moq, and complete the
/// handshake (a `200 OK` for WebTransport). Returns the established session plus the
/// request URL and validated mTLS identity, both captured before the response consumes
/// the request. Raw QUIC carries no request URL (the path rides the SETUP instead).
pub(crate) async fn accept(
	conn: quinn::Incoming,
	alpns: Vec<&'static str>,
) -> Result<(
	web_transport_quinn::Session,
	Option<Url>,
	Option<crate::tls::PeerIdentity>,
)> {
	let mut conn = conn.accept()?;

	let handshake = conn
		.handshake_data()
		.await?
		.downcast::<quinn::crypto::rustls::HandshakeData>()
		.unwrap();

	let alpn = handshake.protocol.ok_or(Error::MissingAlpn)?;
	let alpn = String::from_utf8(alpn)?;
	let host = handshake.server_name.unwrap_or_default();

	tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepting");

	// Wait for the QUIC connection to be established.
	let conn = conn.await.map_err(Error::Establish)?;

	let span = tracing::Span::current();
	span.record("id", conn.stable_id()); // TODO can we get this earlier?
	tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepted");

	match alpn.as_str() {
		web_transport_quinn::ALPN => {
			// Wait for the CONNECT request, then capture its URL and mTLS identity before
			// the response consumes it.
			let request = web_transport_quinn::Request::accept(conn)
				.await
				.map_err(Error::RecvRequest)?;
			let url = Some(request.url.clone());
			let identity = crate::tls::PeerIdentity::from_any(request.conn().peer_identity());

			let mut response = web_transport_quinn::proto::ConnectResponse::OK;
			// Pick the first sub-protocol that we actually support.
			// This is the WebTransport equivalent of ALPN negotiation.
			// If no match is found, we default to no sub-protocol to support older
			// clients that don't use ALPN. We assume moq-transport-14/moq-lite-02
			// and perform the SETUP_x exchange instead.
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
			// Raw QUIC carries no in-band request URL like WebTransport's CONNECT, so the TLS
			// SNI is the only authority the client can offer, and it's optional. A client dialing
			// a bare IP sends no SNI (RFC 6066 forbids IP literals), leaving `host` empty; the
			// resulting hostless `moqt://` routes to the root path, exactly like a URL-less stream
			// transport. `url()` returns `None` for the raw variant either way.
			let host_str = if host.contains(':') {
				format!("[{}]", host)
			} else {
				host.clone()
			};
			let url = format!("moqt://{}", host_str).parse::<Url>().map_err(Error::BuildUrl)?;
			let request = web_transport_quinn::proto::ConnectRequest::new(url);
			let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(alpn);
			let identity = crate::tls::PeerIdentity::from_any(conn.peer_identity());
			// Raw QUIC carries no request URL; the path rides the SETUP.
			let session = web_transport_quinn::Session::raw(conn, request, response);
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

impl quinn::ConnectionIdGenerator for ServerIdGenerator {
	fn generate_cid(&mut self) -> quinn::ConnectionId {
		use rand::RngExt;
		let cid_len = self.cid_len();
		let mut cid = Vec::with_capacity(cid_len);
		// First byte has "self-encoded length" of server ID + nonce
		cid.push((cid_len - 1) as u8);
		cid.extend(self.server_id.0.iter());
		cid.extend(rand::rng().random_iter::<u8>().take(self.nonce_len));
		quinn::ConnectionId::new(cid.as_slice())
	}

	fn cid_len(&self) -> usize {
		1 + self.server_id.len() + self.nonce_len
	}

	fn cid_lifetime(&self) -> Option<Duration> {
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Build a controller from each family's factory and downcast it to the
	/// concrete quinn implementation it must map to.
	#[test]
	fn congestion_factory_maps_each_family() {
		let now = std::time::Instant::now();
		let mtu = 1200;

		let loss = congestion_factory(CongestionControl::Loss).build(now, mtu);
		assert!(loss.into_any().downcast::<quinn::congestion::Cubic>().is_ok());

		let delay = congestion_factory(CongestionControl::Delay).build(now, mtu);
		assert!(delay.into_any().downcast::<quinn::congestion::Bbr>().is_ok());
	}

	/// Loopback regression test: a config selecting BBR must produce live
	/// connections that actually run quinn's BBR controller, on both ends.
	#[tokio::test]
	async fn delay_reaches_the_live_connection() {
		let server_config = ServerConfig {
			bind: Some("127.0.0.1:0".to_string()),
			tls: crate::tls::Server {
				generate: vec!["localhost".into()],
				..Default::default()
			},
			quic: crate::quic::Server {
				congestion_control: Some(CongestionControl::Delay),
				..Default::default()
			},
			..Default::default()
		};

		let server = QuinnServer::new(server_config).expect("server init");
		let addr = server.local_addr().expect("local addr");

		let accepted = tokio::spawn(async move {
			let incoming = server.accept().await.expect("no incoming connection");
			let conn = incoming.accept().expect("accept").await.expect("handshake");
			conn.congestion_state()
				.into_any()
				.downcast::<quinn::congestion::Bbr>()
				.is_ok()
		});

		// tls::Client has a private field, so it can't be built with a struct literal.
		let mut tls_config = crate::tls::Client::default();
		tls_config.disable_verify = Some(true);

		let client_config = ClientConfig {
			bind: "127.0.0.1:0".parse().unwrap(),
			tls: tls_config,
			quic: crate::quic::Client {
				congestion_control: Some(CongestionControl::Delay),
				..Default::default()
			},
			..Default::default()
		};

		let tls = client_config.tls.build().expect("tls config");
		let client = QuinnClient::new(&client_config).expect("client init");
		// Dial the loopback IP directly so the system resolver is never involved.
		let url: Url = format!("moqt://127.0.0.1:{}", addr.port()).parse().unwrap();

		// Bound the whole connect + accept + assert flow so a handshake
		// regression fails fast instead of stalling CI.
		tokio::time::timeout(Duration::from_secs(5), async move {
			let session = client
				.connect(&tls, url, &moq_net::Versions::default())
				.await
				.expect("connect failed");

			// web_transport_quinn::Session derefs to the quinn connection.
			assert!(
				session
					.congestion_state()
					.into_any()
					.downcast::<quinn::congestion::Bbr>()
					.is_ok(),
				"client connection is not running BBR"
			);
			assert!(
				accepted.await.expect("server task panicked"),
				"server connection is not running BBR"
			);
		})
		.await
		.expect("test timed out");
	}
}
