use crate::client::ClientConfig;
use crate::crypto;
use crate::server::{ServerConfig, ServerId, ServerTlsConfig, ServerTlsInfo};
use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::{net, time};
use url::Url;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuinnClient {
	pub quic: quinn::Endpoint,
	pub transport: Arc<quinn::TransportConfig>,
}

impl QuinnClient {
	pub fn new(config: &ClientConfig) -> anyhow::Result<Self> {
		let socket = std::net::UdpSocket::bind(config.bind).context("failed to bind UDP socket")?;

		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(time::Duration::from_secs(10).try_into().unwrap()));
		transport.keep_alive_interval(Some(time::Duration::from_secs(4)));
		transport.mtu_discovery_config(None); // Disable MTU discovery

		let max_streams = config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS);
		let max_streams = quinn::VarInt::from_u64(max_streams).unwrap_or(quinn::VarInt::MAX);
		transport.max_concurrent_bidi_streams(max_streams);
		transport.max_concurrent_uni_streams(max_streams);

		let transport = Arc::new(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().context("no async runtime")?;
		let endpoint_config = quinn::EndpointConfig::default();

		// Create the generic QUIC endpoint.
		let quic =
			quinn::Endpoint::new(endpoint_config, None, socket, runtime).context("failed to create QUIC endpoint")?;

		Ok(Self { quic, transport })
	}

	pub async fn connect(&self, tls: &rustls::ClientConfig, url: Url) -> anyhow::Result<web_transport_quinn::Session> {
		let mut url = url;
		let mut config = tls.clone();

		let host = url.host().context("invalid DNS name")?.to_string();
		let port = url.port().unwrap_or(443);

		// Look up the DNS entry.
		let ip = tokio::net::lookup_host((host.clone(), port))
			.await
			.context("failed DNS lookup")?
			.next()
			.context("no DNS entries")?;

		if url.scheme() == "http" {
			// Perform a HTTP request to fetch the certificate fingerprint.
			let mut fingerprint = url.clone();
			fingerprint.set_path("/certificate.sha256");
			fingerprint.set_query(None);
			fingerprint.set_fragment(None);

			tracing::warn!(url = %fingerprint, "performing insecure HTTP request for certificate");

			let resp = reqwest::get(fingerprint.as_str())
				.await
				.context("failed to fetch fingerprint")?
				.error_for_status()
				.context("fingerprint request failed")?;

			let fingerprint = resp.text().await.context("failed to read fingerprint")?;
			let fingerprint = hex::decode(fingerprint.trim()).context("invalid fingerprint")?;

			let verifier = FingerprintVerifier::new(config.crypto_provider().clone(), fingerprint);
			config.dangerous().set_certificate_verifier(Arc::new(verifier));

			url.set_scheme("https").expect("failed to set scheme");
		}

		let alpns = match url.scheme() {
			"https" => vec![web_transport_quinn::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => moq_lite::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect(),
			_ => anyhow::bail!("url scheme must be 'https', 'moqt', or 'moql'"),
		};

		config.alpn_protocols = alpns;
		config.key_log = Arc::new(rustls::KeyLogFile::new());

		let config: quinn::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = quinn::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(%url, %ip, "connecting");

		let connection = self.quic.connect_with(config, ip, &host)?.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let mut request = web_transport_quinn::proto::ConnectRequest::new(url.clone());
		for alpn in moq_lite::ALPNS {
			request = request.with_protocol(alpn.to_string());
		}

		let session = match url.scheme() {
			"https" => web_transport_quinn::Session::connect(connection, request).await?,
			"moqt" | "moql" => {
				let handshake = connection
					.handshake_data()
					.context("missing handshake data")?
					.downcast::<quinn::crypto::rustls::HandshakeData>()
					.unwrap();

				let alpn = handshake.protocol.context("missing ALPN")?;
				let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;

				let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(alpn);
				web_transport_quinn::Session::raw(connection, request, response)
			}
			_ => anyhow::bail!("unsupported URL scheme: {}", url.scheme()),
		};

		Ok(session)
	}
}

// ── FingerprintVerifier ─────────────────────────────────────────────

#[derive(Debug)]
struct FingerprintVerifier {
	provider: crypto::Provider,
	fingerprint: Vec<u8>,
}

impl FingerprintVerifier {
	pub fn new(provider: crypto::Provider, fingerprint: Vec<u8>) -> Self {
		Self { provider, fingerprint }
	}
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
	fn verify_server_cert(
		&self,
		end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp: &[u8],
		_now: UnixTime,
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		let fingerprint = crypto::sha256(&self.provider, end_entity);
		if fingerprint.as_ref() == self.fingerprint.as_slice() {
			Ok(rustls::client::danger::ServerCertVerified::assertion())
		} else {
			Err(rustls::Error::General("fingerprint mismatch".into()))
		}
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.provider.signature_verification_algorithms.supported_schemes()
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct QuinnServer {
	pub quic: quinn::Endpoint,
	pub certs: Arc<ServeCerts>,
}

impl QuinnServer {
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		// Enable BBR congestion control
		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
		transport.keep_alive_interval(Some(Duration::from_secs(4)));
		transport.mtu_discovery_config(None); // Disable MTU discovery

		let max_streams = config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS);
		let max_streams = quinn::VarInt::from_u64(max_streams).unwrap_or(quinn::VarInt::MAX);
		transport.max_concurrent_bidi_streams(max_streams);
		transport.max_concurrent_uni_streams(max_streams);

		let transport = Arc::new(transport);

		let provider = crypto::provider();

		let certs = ServeCerts::new(provider.clone());
		certs.load_certs(&config.tls)?;
		let certs = Arc::new(certs);

		#[cfg(unix)]
		tokio::spawn(reload_certs(certs.clone(), config.tls.clone()));

		let mut tls = rustls::ServerConfig::builder_with_provider(provider)
			.with_protocol_versions(&[&rustls::version::TLS13])?
			.with_no_client_auth()
			.with_cert_resolver(certs.clone());

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = moq_lite::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect();
		alpns.push(web_transport_quinn::ALPN.as_bytes().to_vec());

		tls.alpn_protocols = alpns;
		tls.key_log = Arc::new(rustls::KeyLogFile::new());

		let tls: quinn::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut tls = quinn::ServerConfig::with_crypto(Arc::new(tls));
		tls.transport_config(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().context("no async runtime")?;

		// Configure connection ID generator with server ID if provided
		let mut endpoint_config = quinn::EndpointConfig::default();
		if let Some(server_id) = config.quic_lb_id {
			let nonce_len = config.quic_lb_nonce.unwrap_or(8);
			anyhow::ensure!(nonce_len >= 4, "quic_lb_nonce must be at least 4");

			let cid_len = 1 + server_id.len() + nonce_len;
			anyhow::ensure!(cid_len <= 20, "connection ID length ({cid_len}) exceeds maximum of 20");

			tracing::info!(
				?server_id,
				nonce_len,
				"using QUIC-LB compatible connection ID generation"
			);
			endpoint_config.cid_generator(move || Box::new(ServerIdGenerator::new(server_id.clone(), nonce_len)));
		}

		let listen = config.bind.unwrap_or("[::]:443".parse().unwrap());
		let socket = std::net::UdpSocket::bind(listen).context("failed to bind UDP socket")?;

		// Create the generic QUIC endpoint.
		let quic = quinn::Endpoint::new(endpoint_config, Some(tls), socket, runtime)
			.context("failed to create QUIC endpoint")?;

		Ok(Self { quic, certs })
	}

	pub fn accept(&self) -> impl std::future::Future<Output = Option<quinn::Incoming>> + '_ {
		self.quic.accept()
	}

	pub fn tls_info(&self) -> Arc<RwLock<ServerTlsInfo>> {
		self.certs.info.clone()
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		self.quic.local_addr().context("failed to get local address")
	}

	pub fn close(&self) {
		self.quic.close(quinn::VarInt::from_u32(0), b"server shutdown");
	}
}

// ── QuinnRequest ────────────────────────────────────────────────────

/// A raw QUIC connection request without WebTransport framing (quinn backend).
pub(crate) enum QuinnRequest {
	Raw {
		request: web_transport_quinn::proto::ConnectRequest,
		response: web_transport_quinn::proto::ConnectResponse,
		connection: quinn::Connection,
	},
	WebTransport {
		request: web_transport_quinn::Request,
	},
}

impl QuinnRequest {
	pub async fn accept(conn: quinn::Incoming) -> anyhow::Result<Self> {
		let mut conn = conn.accept()?;

		let handshake = conn
			.handshake_data()
			.await?
			.downcast::<quinn::crypto::rustls::HandshakeData>()
			.unwrap();

		let alpn = handshake.protocol.context("missing ALPN")?;
		let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;
		let host = handshake.server_name.unwrap_or_default();

		tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepting");

		// Wait for the QUIC connection to be established.
		let conn = conn.await.context("failed to establish QUIC connection")?;

		let span = tracing::Span::current();
		span.record("id", conn.stable_id()); // TODO can we get this earlier?
		tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepted");

		match alpn.as_str() {
			web_transport_quinn::ALPN => {
				// Wait for the CONNECT request.
				let request = web_transport_quinn::Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Self::WebTransport { request })
			}
			alpn if moq_lite::ALPNS.contains(&alpn) => {
				let url = format!("moqt://{}", host).parse::<Url>().unwrap();
				let request = web_transport_quinn::proto::ConnectRequest::new(url);
				let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(alpn);
				Ok(Self::Raw {
					connection: conn,
					request,
					response,
				})
			}
			_ => anyhow::bail!("unsupported ALPN: {alpn}"),
		}
	}

	/// Accept the session, returning a 200 OK if using WebTransport.
	pub async fn ok(self) -> Result<web_transport_quinn::Session, web_transport_quinn::ServerError> {
		match self {
			QuinnRequest::Raw {
				connection,
				request,
				response,
			} => Ok(web_transport_quinn::Session::raw(connection, request, response)),
			QuinnRequest::WebTransport { request } => {
				let mut response = web_transport_quinn::proto::ConnectResponse::OK;
				// TODO actually pick a valid moq protocol
				if let Some(alpn) = request.protocols.first() {
					response = response.with_protocol(alpn);
				}
				request.respond(response).await
			}
		}
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> Option<&Url> {
		match self {
			QuinnRequest::Raw { .. } => None,
			QuinnRequest::WebTransport { request } => Some(&request.url),
		}
	}

	/// Reject the session with a status code.
	pub async fn close(
		self,
		status: web_transport_quinn::http::StatusCode,
	) -> Result<(), web_transport_quinn::ServerError> {
		match self {
			QuinnRequest::Raw { connection, .. } => {
				connection.close(status.as_u16().into(), status.as_str().as_bytes());
				Ok(())
			}
			QuinnRequest::WebTransport { request, .. } => request.reject(status).await,
		}
	}
}

// ── ServeCerts ──────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct ServeCerts {
	pub info: Arc<RwLock<ServerTlsInfo>>,
	provider: crypto::Provider,
}

impl ServeCerts {
	pub fn new(provider: crypto::Provider) -> Self {
		Self {
			info: Arc::new(RwLock::new(ServerTlsInfo {
				certs: Vec::new(),
				fingerprints: Vec::new(),
			})),
			provider,
		}
	}

	pub fn load_certs(&self, config: &ServerTlsConfig) -> anyhow::Result<()> {
		anyhow::ensure!(config.cert.len() == config.key.len(), "must provide both cert and key");

		let mut certs = Vec::new();

		// Load the certificate and key files based on their index.
		for (cert, key) in config.cert.iter().zip(config.key.iter()) {
			certs.push(Arc::new(self.load(cert, key)?));
		}

		// Generate a new certificate if requested.
		if !config.generate.is_empty() {
			certs.push(Arc::new(self.generate(&config.generate)?));
		}

		self.set_certs(certs);
		Ok(())
	}

	// Load a certificate and corresponding key from a file, but don't add it to the certs
	fn load(&self, chain_path: &PathBuf, key_path: &PathBuf) -> anyhow::Result<rustls::sign::CertifiedKey> {
		let chain = fs::File::open(chain_path).context("failed to open cert file")?;
		let mut chain = io::BufReader::new(chain);

		let chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut chain)
			.collect::<Result<_, _>>()
			.context("failed to read certs")?;

		anyhow::ensure!(!chain.is_empty(), "could not find certificate");

		// Read the PEM private key
		let mut keys = fs::File::open(key_path).context("failed to open key file")?;

		// Read the keys into a Vec so we can parse it twice.
		let mut buf = Vec::new();
		keys.read_to_end(&mut buf)?;

		let key = rustls_pemfile::private_key(&mut Cursor::new(&buf))?.context("missing private key")?;
		let key = self.provider.key_provider.load_private_key(key)?;

		let certified_key = rustls::sign::CertifiedKey::new(chain, key);

		certified_key.keys_match().context(format!(
			"private key {} doesn't match certificate {}",
			key_path.display(),
			chain_path.display()
		))?;

		Ok(certified_key)
	}

	#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
	fn generate(&self, hostnames: &[String]) -> anyhow::Result<rustls::sign::CertifiedKey> {
		let key_pair = rcgen::KeyPair::generate()?;

		let mut params = rcgen::CertificateParams::new(hostnames)?;

		// Make the certificate valid for two weeks, starting yesterday (in case of clock drift).
		// WebTransport certificates MUST be valid for two weeks at most.
		params.not_before = ::time::OffsetDateTime::now_utc() - ::time::Duration::days(1);
		params.not_after = params.not_before + ::time::Duration::days(14);

		// Generate the certificate
		let cert = params.self_signed(&key_pair)?;

		// Convert the rcgen type to the rustls type.
		let key_der = key_pair.serialized_der().to_vec();
		let key_der = PrivatePkcs8KeyDer::from(key_der);
		let key = self.provider.key_provider.load_private_key(key_der.into())?;

		// Create a rustls::sign::CertifiedKey
		Ok(rustls::sign::CertifiedKey::new(vec![cert.into()], key))
	}

	#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
	fn generate(&self, hostnames: &[String]) -> anyhow::Result<rustls::sign::CertifiedKey> {
		anyhow::bail!("no crypto provider available; enable aws-lc-rs or ring feature");
	}

	// Replace the certificates
	pub fn set_certs(&self, certs: Vec<Arc<rustls::sign::CertifiedKey>>) {
		let fingerprints = certs
			.iter()
			.map(|ck| {
				let fingerprint = crate::crypto::sha256(&self.provider, ck.cert[0].as_ref());
				hex::encode(fingerprint)
			})
			.collect();

		let mut info = self.info.write().expect("info write lock poisoned");
		info.certs = certs;
		info.fingerprints = fingerprints;
	}

	// Return the best certificate for the given ClientHello.
	fn best_certificate(
		&self,
		client_hello: &rustls::server::ClientHello<'_>,
	) -> Option<Arc<rustls::sign::CertifiedKey>> {
		let server_name = client_hello.server_name()?;
		let dns_name = rustls::pki_types::ServerName::try_from(server_name).ok()?;

		for ck in self.info.read().expect("info read lock poisoned").certs.iter() {
			let leaf: webpki::EndEntityCert = ck
				.end_entity_cert()
				.expect("missing certificate")
				.try_into()
				.expect("failed to parse certificate");

			if leaf.verify_is_valid_for_subject_name(&dns_name).is_ok() {
				return Some(ck.clone());
			}
		}

		None
	}
}

impl rustls::server::ResolvesServerCert for ServeCerts {
	fn resolve(&self, client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
		if let Some(cert) = self.best_certificate(&client_hello) {
			return Some(cert);
		}

		// If this happens, it means the client was trying to connect to an unknown hostname.
		// We do our best and return the first certificate.
		tracing::warn!(server_name = ?client_hello.server_name(), "no SNI certificate found");

		self.info
			.read()
			.expect("info read lock poisoned")
			.certs
			.first()
			.cloned()
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
		use rand::Rng;
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

// ── reload_certs (unix) ─────────────────────────────────────────────

#[cfg(unix)]
pub(crate) async fn reload_certs(certs: Arc<ServeCerts>, tls_config: ServerTlsConfig) {
	use tokio::signal::unix::{SignalKind, signal};

	// Dunno why we wouldn't be allowed to listen for signals, but just in case.
	let mut listener = signal(SignalKind::user_defined1()).expect("failed to listen for signals");

	while listener.recv().await.is_some() {
		tracing::info!("reloading server certificates");

		if let Err(err) = certs.load_certs(&tls_config) {
			tracing::warn!(%err, "failed to reload server certificates");
		}
	}
}
