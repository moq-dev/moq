use crate::crypto;
use anyhow::Context;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::RootCertStore;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::{fs, io, net, sync::Arc, time};
use url::Url;
use web_transport_ws::{tokio_tungstenite, tungstenite};

// Track servers (hostname:port) where WebSocket won the race, so we won't give QUIC a headstart next time
static WEBSOCKET_WON: LazyLock<Mutex<HashSet<(String, u16)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Clone, Default, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientTls {
	/// Use the TLS root at this path, encoded as PEM.
	///
	/// This value can be provided multiple times for multiple roots.
	/// If this is empty, system roots will be used instead
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "tls-root", long = "tls-root", env = "MOQ_CLIENT_TLS_ROOT")]
	pub root: Vec<PathBuf>,

	/// Danger: Disable TLS certificate verification.
	///
	/// Fine for local development and between relays, but should be used in caution in production.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "tls-disable-verify",
		long = "tls-disable-verify",
		env = "MOQ_CLIENT_TLS_DISABLE_VERIFY",
		action = clap::ArgAction::SetTrue
	)]
	pub disable_verify: Option<bool>,
}

#[derive(Clone, Default, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientWebSocket {
	/// Delay in milliseconds before attempting WebSocket fallback (default: 200)
	/// If WebSocket won the previous race for a given server, this will be 0.
	#[arg(
		id = "websocket-delay",
		long = "websocket-delay",
		env = "MOQ_CLIENT_WEBSOCKET_DELAY",
		default_value = "200ms",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub delay: Option<time::Duration>,
}

#[derive(Clone, Debug, clap::Parser, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClientConfig {
	/// Listen for UDP packets on the given address.
	#[arg(
		id = "client-bind",
		long = "client-bind",
		default_value = "[::]:0",
		env = "MOQ_CLIENT_BIND"
	)]
	pub bind: net::SocketAddr,

	#[command(flatten)]
	#[serde(default)]
	pub tls: ClientTls,

	#[command(flatten)]
	#[serde(default)]
	pub websocket: ClientWebSocket,
}

impl Default for ClientConfig {
	fn default() -> Self {
		Self {
			bind: "[::]:0".parse().unwrap(),
			tls: ClientTls::default(),
			websocket: ClientWebSocket::default(),
		}
	}
}

impl ClientConfig {
	pub fn init(self) -> anyhow::Result<Client> {
		Client::new(self)
	}
}

#[derive(Clone)]
pub struct Client {
	pub quic: quinn::Endpoint,
	pub tls: rustls::ClientConfig,
	pub transport: Arc<quinn::TransportConfig>,
	pub websocket_delay: Option<time::Duration>,
}

impl Client {
	pub fn new(config: ClientConfig) -> anyhow::Result<Self> {
		let provider = crypto::provider();

		// Create a list of acceptable root certificates.
		let mut roots = RootCertStore::empty();

		if config.tls.root.is_empty() {
			let native = rustls_native_certs::load_native_certs();

			// Log any errors that occurred while loading the native root certificates.
			for err in native.errors {
				tracing::warn!(%err, "failed to load root cert");
			}

			// Add the platform's native root certificates.
			for cert in native.certs {
				roots.add(cert).context("failed to add root cert")?;
			}
		} else {
			// Add the specified root certificates.
			for root in &config.tls.root {
				let root = fs::File::open(root).context("failed to open root cert file")?;
				let mut root = io::BufReader::new(root);

				let root = rustls_pemfile::certs(&mut root)
					.next()
					.context("no roots found")?
					.context("failed to read root cert")?;

				roots.add(root).context("failed to add root cert")?;
			}
		}

		// Create the TLS configuration we'll use as a client (relay -> relay)
		let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])?
			.with_root_certificates(roots)
			.with_no_client_auth();

		// Allow disabling TLS verification altogether.
		if config.tls.disable_verify.unwrap_or_default() {
			tracing::warn!("TLS server certificate verification is disabled; A man-in-the-middle attack is possible.");

			let noop = NoCertificateVerification(provider.clone());
			tls.dangerous().set_certificate_verifier(Arc::new(noop));
		}

		let socket = std::net::UdpSocket::bind(config.bind).context("failed to bind UDP socket")?;

		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(time::Duration::from_secs(10).try_into().unwrap()));
		transport.keep_alive_interval(Some(time::Duration::from_secs(4)));
		//transport.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
		transport.mtu_discovery_config(None); // Disable MTU discovery
		let transport = Arc::new(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().context("no async runtime")?;
		let endpoint_config = quinn::EndpointConfig::default();

		// Create the generic QUIC endpoint.
		let quic =
			quinn::Endpoint::new(endpoint_config, None, socket, runtime).context("failed to create QUIC endpoint")?;

		Ok(Self {
			quic,
			tls,
			transport,
			websocket_delay: config.websocket.delay,
		})
	}

	/// Establish a WebTransport/QUIC connection followed by a MoQ handshake.
	pub async fn connect(
		&self,
		url: Url,
		publish: impl Into<Option<moq_lite::OriginConsumer>>,
		subscribe: impl Into<Option<moq_lite::OriginProducer>>,
	) -> anyhow::Result<moq_lite::Session> {
		let session = self.connect_quic(url).await?;
		let session = moq_lite::Session::connect(session, publish, subscribe).await?;
		Ok(session)
	}

	/// Establish a WebTransport/QUIC connection or a WebSocket connection, whichever is available first.
	///
	/// Establishes a MoQ handshake on the winning transport.
	pub async fn connect_with_fallback(
		&self,
		url: Url,
		publish: impl Into<Option<moq_lite::OriginConsumer>>,
		subscribe: impl Into<Option<moq_lite::OriginProducer>>,
	) -> anyhow::Result<moq_lite::Session> {
		// Create futures for both possible protocols
		let quic_url = url.clone();
		let quic_handle = async {
			match self.connect_quic(quic_url).await {
				Ok(session) => Some(session),
				Err(err) => {
					tracing::warn!(%err, "QUIC connection failed");
					None
				}
			}
		};

		let ws_handle = async {
			match self.connect_websocket(url).await {
				Ok(session) => Some(session),
				Err(err) => {
					tracing::warn!(%err, "WebSocket connection failed");
					None
				}
			}
		};

		// Race the connection futures
		Ok(tokio::select! {
			Some(quic) = quic_handle => moq_lite::Session::connect(quic, publish, subscribe).await?,
			Some(ws) = ws_handle => moq_lite::Session::connect(ws, publish, subscribe).await?,
			// If both attempts fail, return an error
			else => anyhow::bail!("failed to connect to server"),
		})
	}

	async fn connect_quic(&self, mut url: Url) -> anyhow::Result<web_transport_quinn::Session> {
		let mut config = self.tls.clone();

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

		let alpn = match url.scheme() {
			"https" => web_transport_quinn::ALPN,
			"moql" => moq_lite::lite::ALPN,
			"moqt" => moq_lite::ietf::ALPN,
			_ => anyhow::bail!("url scheme must be 'http', 'https', or 'moql'"),
		};

		// TODO support connecting to both ALPNs at the same time
		config.alpn_protocols = vec![alpn.as_bytes().to_vec()];
		config.key_log = Arc::new(rustls::KeyLogFile::new());

		let config: quinn::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = quinn::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(%url, %ip, %alpn, "connecting");

		let connection = self.quic.connect_with(config, ip, &host)?.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let session = match alpn {
			web_transport_quinn::ALPN => web_transport_quinn::Session::connect(connection, url).await?,
			moq_lite::lite::ALPN | moq_lite::ietf::ALPN => web_transport_quinn::Session::raw(connection, url),
			_ => unreachable!("ALPN was checked above"),
		};

		Ok(session)
	}

	async fn connect_websocket(&self, mut url: Url) -> anyhow::Result<web_transport_ws::Session> {
		let host = url.host_str().context("missing hostname")?.to_string();
		let port = url.port().unwrap_or_else(|| match url.scheme() {
			"https" | "wss" | "moql" | "moqt" => 443,
			"http" | "ws" => 80,
			_ => 443,
		});
		let key = (host, port);

		// Apply a small penalty to WebSocket to improve odds for QUIC to connect first,
		// unless we've already had to fall back to WebSockets for this server.
		// TODO if let chain
		match self.websocket_delay {
			Some(delay) if !WEBSOCKET_WON.lock().unwrap().contains(&key) => {
				tokio::time::sleep(delay).await;
				tracing::debug!(%url, delay_ms = %delay.as_millis(), "QUIC not yet connected, attempting WebSocket fallback");
			}
			_ => {}
		}

		// Convert URL scheme: http:// -> ws://, https:// -> wss://
		match url.scheme() {
			"http" => {
				url.set_scheme("ws").expect("failed to set scheme");
			}
			"https" | "moql" | "moqt" => {
				url.set_scheme("wss").expect("failed to set scheme");
			}
			"ws" | "wss" => {}
			_ => anyhow::bail!("unsupported URL scheme for WebSocket: {}", url.scheme()),
		};

		tracing::debug!(%url, "connecting via WebSocket");

		// Connect using tokio-tungstenite
		let (ws_stream, _response) = tokio_tungstenite::connect_async_with_config(
			url.as_str(),
			Some(tungstenite::protocol::WebSocketConfig {
				max_message_size: Some(64 << 20), // 64 MB
				max_frame_size: Some(16 << 20),   // 16 MB
				accept_unmasked_frames: false,
				..Default::default()
			}),
			false, // disable_nagle
		)
		.await
		.context("failed to connect WebSocket")?;

		// Wrap WebSocket in WebTransport compatibility layer
		// Similar to what the relay does: web_transport_ws::Session::new(socket, true)
		let session = web_transport_ws::Session::new(ws_stream, false);

		tracing::warn!(%url, "using WebSocket fallback");
		WEBSOCKET_WON.lock().unwrap().insert(key);

		Ok(session)
	}
}

#[derive(Debug)]
struct NoCertificateVerification(crypto::Provider);

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
	fn verify_server_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp: &[u8],
		_now: UnixTime,
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		Ok(rustls::client::danger::ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		cert: &CertificateDer<'_>,
		dss: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		self.0.signature_verification_algorithms.supported_schemes()
	}
}

// Verify the certificate matches a provided fingerprint.
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
