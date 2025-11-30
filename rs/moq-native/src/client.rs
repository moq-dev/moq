use crate::crypto;
use anyhow::Context;
use once_cell::sync::Lazy;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::RootCertStore;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::{fs, io, net, sync::Arc, time};
use url::Url;
use web_transport_any::WebTransportSessionAny;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerCacheKey(String, u16);

impl std::fmt::Display for ServerCacheKey {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}:{}", self.0, self.1)
	}
}

// Track servers (hostname:port) where WebSocket won the race, so we won't give QUIC a headstart next time
// Keyed by "hostname:port" string (e.g., "relay.example.com:443")
static WEBSOCKET_WON: Lazy<Mutex<HashSet<ServerCacheKey>>> = Lazy::new(|| Mutex::new(HashSet::new()));

// Helper function to extract hostname:port key from URL
fn server_key(url: &Url) -> Option<ServerCacheKey> {
	let host = url.host_str()?;
	let port = url.port().unwrap_or_else(|| match url.scheme() {
		"https" | "wss" | "moql" | "moqt" => 443,
		"http" | "ws" => 80,
		_ => 443,
	});
	Some(ServerCacheKey(host.to_string(), port))
}

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
		default_value = "200ms"
	)]
	#[serde(deserialize_with = "deserialize_humantime")]
	#[serde(serialize_with = "serialize_humantime")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub delay: Option<humantime::Duration>,
}

fn deserialize_humantime<'de, D>(deserializer: D) -> Result<Option<humantime::Duration>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	let buf = <String as serde::Deserialize>::deserialize(deserializer)?;

	buf.parse::<humantime::Duration>()
		.map_err(serde::de::Error::custom)
		.map(Some)
}

fn serialize_humantime<S>(duration: &Option<humantime::Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
	S: serde::Serializer,
{
	<String as serde::Serialize>::serialize(&duration.unwrap_or_default().to_string(), serializer)
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
	pub websocket_delay: Option<std::time::Duration>,
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
			websocket_delay: config.websocket.delay.map(Into::into),
		})
	}

	pub async fn connect(&self, url: Url) -> anyhow::Result<web_transport_quinn::Session> {
		self.connect_quic(url).await
	}

	pub async fn connect_with_fallback(&self, url: Url) -> anyhow::Result<WebTransportSessionAny> {
		// Capture QUIC error so it can be used if both transports fail
		let mut quic_error: Option<anyhow::Error> = None;

		// Create futures for both possible protocols
		let quic_url = url.clone();
		let quic_handle = async {
			match self.connect_quic(quic_url).await {
				Ok(session) => Some(session),
				Err(err) => {
					quic_error = Some(err);
					None
				}
			}
		};

		let ws_handle = async {
			let cache_key = server_key(&url);

			// Apply a small penalty to WebSocket to improve odds for QUIC to connect first,
			// unless we've already had to fall back to WebSockets for this server.
			let websocket_penalty = match &cache_key {
				Some(key) if !WEBSOCKET_WON.lock().unwrap().contains(key) => self.websocket_delay,
				_ => None,
			};

			if let Some(delay) = websocket_penalty {
				tokio::time::sleep(delay).await;
				tracing::debug!(url = %url, delay_ms = %delay.as_millis(), "QUIC not yet connected, attempting WebSocket fallback");
			}

			match self.connect_websocket(url).await {
				Ok(session) => {
					if let Some(cache_key) = cache_key {
						tracing::warn!(server = %cache_key, "using WebSocket fallback");
						WEBSOCKET_WON.lock().unwrap().insert(cache_key);
					}
					Some(session)
				}
				Err(err) => {
					tracing::debug!(%err, "WebSocket connection failed");
					None
				}
			}
		};

		// Race the connection futures
		tokio::select! {
			Some(quic_session) = quic_handle => Ok(quic_session.into()),
			Some(ws_session) = ws_handle => Ok(ws_session.into()),
			// If both attempts fail, return the QUIC error (if available)
			else => Err(quic_error.unwrap_or_else(|| anyhow::Error::msg("unknown error"))),
		}
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
		// Convert URL scheme: http:// -> ws://, https:// -> wss://
		let ws_url = match url.scheme() {
			"http" => {
				url.set_scheme("ws").expect("failed to set scheme");
				url
			}
			"https" | "moql" | "moqt" => {
				url.set_scheme("wss").expect("failed to set scheme");
				url
			}
			"ws" | "wss" => url,
			_ => anyhow::bail!("unsupported URL scheme for WebSocket: {}", url.scheme()),
		};

		tracing::debug!(url = %ws_url, "connecting via WebSocket");

		// Connect using tokio-tungstenite
		let (ws_stream, _response) = tokio_tungstenite::connect_async_with_config(
			ws_url.as_str(),
			Some(tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
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
