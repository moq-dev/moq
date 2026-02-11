use crate::crypto;
use anyhow::Context;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::{fs, io, net, sync::Arc, time};
use url::Url;
#[cfg(feature = "iroh")]
use web_transport_iroh::iroh;
use web_transport_ws::{tokio_tungstenite, tungstenite};

// Track servers (hostname:port) where WebSocket won the race, so we won't give QUIC a headstart next time
static WEBSOCKET_WON: LazyLock<Mutex<HashSet<(String, u16)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// TLS configuration for the client.
#[derive(Clone, Default, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
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
		default_missing_value = "true",
		num_args = 0..=1,
		value_parser = clap::value_parser!(bool),
	)]
	pub disable_verify: Option<bool>,
}

/// WebSocket configuration for the client.
#[derive(Clone, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ClientWebSocket {
	/// Whether to enable WebSocket support.
	#[arg(
		id = "websocket-enabled",
		long = "websocket-enabled",
		env = "MOQ_CLIENT_WEBSOCKET_ENABLED",
		default_value = "true"
	)]
	pub enabled: bool,

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

impl Default for ClientWebSocket {
	fn default() -> Self {
		Self {
			enabled: true,
			delay: Some(time::Duration::from_millis(200)),
		}
	}
}

/// Configuration for the MoQ client.
#[derive(Clone, Debug, clap::Parser, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
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

impl ClientConfig {
	pub fn init(self) -> anyhow::Result<Client> {
		Client::new(self)
	}
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

/// Client for establishing MoQ connections over QUIC, WebTransport, or WebSocket.
///
/// Create via [`ClientConfig::init`] or [`Client::new`].
#[derive(Clone)]
#[non_exhaustive]
pub struct Client {
	pub moq: moq_lite::Client,
	pub quic: quinn::Endpoint,
	pub tls: rustls::ClientConfig,
	pub transport: Arc<quinn::TransportConfig>,
	pub websocket: ClientWebSocket,
	#[cfg(feature = "iroh")]
	pub iroh: Option<iroh::Endpoint>,
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
			moq: moq_lite::Client::new(),
			quic,
			tls,
			transport,
			websocket: config.websocket,
			#[cfg(feature = "iroh")]
			iroh: None,
		})
	}

	#[cfg(feature = "iroh")]
	pub fn with_iroh(mut self, iroh: Option<iroh::Endpoint>) -> Self {
		self.iroh = iroh;
		self
	}

	pub fn with_publish(mut self, publish: impl Into<Option<moq_lite::OriginConsumer>>) -> Self {
		self.moq = self.moq.with_publish(publish);
		self
	}

	pub fn with_consume(mut self, consume: impl Into<Option<moq_lite::OriginProducer>>) -> Self {
		self.moq = self.moq.with_consume(consume);
		self
	}

	// TODO: Uncomment when observability feature is merged
	// pub fn with_stats(mut self, stats: impl Into<Option<Arc<dyn moq_lite::Stats>>>) -> Self {
	// 	self.moq = self.moq.with_stats(stats);
	// 	self
	// }

	/// Establish a WebTransport/QUIC connection followed by a MoQ handshake.
	pub async fn connect(&self, url: Url) -> anyhow::Result<moq_lite::Session> {
		#[cfg(feature = "iroh")]
		if crate::iroh::is_iroh_url(&url) {
			let session = self.connect_iroh(url).await?;
			let session = self.moq.connect(session).await?;
			return Ok(session);
		}

		// Create futures for both possible protocols
		let quic_url = url.clone();
		let quic_handle = async {
			let res = self.connect_quic(quic_url).await;
			if let Err(err) = &res {
				tracing::warn!(%err, "QUIC connection failed");
			}
			res
		};

		let ws_handle = async {
			if !self.websocket.enabled {
				return None;
			}

			let res = self.connect_websocket(url).await;
			if let Err(err) = &res {
				tracing::warn!(%err, "WebSocket connection failed");
			}
			Some(res)
		};

		// Race the connection futures
		Ok(tokio::select! {
			Ok(quic) = quic_handle => self.moq.connect(quic).await?,
			Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
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

		let alpns: Vec<String> = match url.scheme() {
			"https" => vec![web_transport_quinn::ALPN.to_string()],
			"moqt" => moq_lite::alpns().iter().map(|alpn| alpn.to_string()).collect(),
			alpn if moq_lite::alpns().contains(&alpn) => vec![alpn.to_string()],
			_ => anyhow::bail!("url scheme must be 'http', 'https', 'moqt', or a recognized MoQ ALPN"),
		};

		config.alpn_protocols = alpns.iter().map(|alpn| alpn.as_bytes().to_vec()).collect();
		config.key_log = Arc::new(rustls::KeyLogFile::new());

		let config: quinn::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = quinn::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(%url, %ip, alpns = ?alpns, "connecting");

		let connection = self.quic.connect_with(config, ip, &host)?.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let mut request = web_transport_quinn::proto::ConnectRequest::new(url);

		let session = if request.url.scheme() == "https" {
			let alpns: Vec<String> = moq_lite::alpns().iter().map(|alpn| alpn.to_string()).collect();
			let request = request.with_protocols(alpns);
			web_transport_quinn::Session::connect(connection, request).await?
		} else {
			request = request.with_protocols(alpns);

			let mut response =
				web_transport_quinn::proto::ConnectResponse::new(web_transport_quinn::http::StatusCode::OK);
			if let Some(negotiated_alpn) = connection
				.handshake_data()
				.and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
				.and_then(|data| data.protocol)
				.and_then(|proto| String::from_utf8(proto).ok())
			{
				response = response.with_protocol(negotiated_alpn);
			}

			web_transport_quinn::Session::raw(connection, request, response)
		};

		Ok(session)
	}

	async fn connect_websocket(&self, mut url: Url) -> anyhow::Result<web_transport_ws::Session> {
		anyhow::ensure!(self.websocket.enabled, "WebSocket support is disabled");

		let host = url.host_str().context("missing hostname")?.to_string();
		let port = url.port().unwrap_or_else(|| match url.scheme() {
			"http" | "ws" => 80,
			_ => 443,
		});
		let key = (host, port);

		// Apply a small penalty to WebSocket to improve odds for QUIC to connect first,
		// unless we've already had to fall back to WebSockets for this server.
		// TODO if let chain
		match self.websocket.delay {
			Some(delay) if !WEBSOCKET_WON.lock().unwrap().contains(&key) => {
				tokio::time::sleep(delay).await;
				tracing::debug!(%url, delay_ms = %delay.as_millis(), "QUIC not yet connected, attempting WebSocket fallback");
			}
			_ => {}
		}

		// Convert URL scheme: http:// -> ws://, https:// -> wss://
		let needs_tls = match url.scheme() {
			"http" => {
				url.set_scheme("ws").expect("failed to set scheme");
				false
			}
			"https" | "moqt" => {
				url.set_scheme("wss").expect("failed to set scheme");
				true
			}
			"ws" => false,
			"wss" => true,
			_ => anyhow::bail!("unsupported URL scheme for WebSocket: {}", url.scheme()),
		};

		tracing::debug!(%url, "connecting via WebSocket");

		// Use the existing TLS config (which respects tls-disable-verify) for secure connections
		let connector = if needs_tls {
			Some(tokio_tungstenite::Connector::Rustls(Arc::new(self.tls.clone())))
		} else {
			None
		};

		// Connect using tokio-tungstenite
		let (ws_stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
			url.as_str(),
			Some(tungstenite::protocol::WebSocketConfig {
				max_message_size: Some(64 << 20), // 64 MB
				max_frame_size: Some(16 << 20),   // 16 MB
				accept_unmasked_frames: false,
				..Default::default()
			}),
			false, // disable_nagle
			connector,
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

	#[cfg(feature = "iroh")]
	async fn connect_iroh(&self, url: Url) -> anyhow::Result<web_transport_iroh::Session> {
		let endpoint = self.iroh.as_ref().context("Iroh support is not enabled")?;
		// TODO Support multiple ALPNs
		let alpn = match url.scheme() {
			"moql+iroh" | "iroh" => moq_lite::lite::ALPN,
			"moqt+iroh" => moq_lite::ietf::ALPN_14,
			"moqt-15+iroh" => moq_lite::ietf::ALPN_15,
			"h3+iroh" => web_transport_iroh::ALPN_H3,
			_ => anyhow::bail!("Invalid URL: unknown scheme"),
		};
		let host = url.host().context("Invalid URL: missing host")?.to_string();
		let endpoint_id: iroh::EndpointId = host.parse().context("Invalid URL: host is not an iroh endpoint id")?;
		let conn = endpoint.connect(endpoint_id, alpn.as_bytes()).await?;
		let session = match alpn {
			web_transport_iroh::ALPN_H3 => {
				// We need to change the scheme to `https` because currently web_transport_iroh only
				// accepts that scheme.
				let url = url_set_scheme(url, "https")?;
				web_transport_iroh::Session::connect_h3(conn, url).await?
			}
			_ => web_transport_iroh::Session::raw(conn),
		};
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

/// Returns a new URL with a changed scheme.
///
/// [`Url::set_scheme`] returns an error if the scheme change is not valid according to
/// [the URL specification's section on legal scheme state overrides](https://url.spec.whatwg.org/#scheme-state).
///
/// This function allows all scheme changes, as long as the resulting URL is valid.
#[cfg(feature = "iroh")]
fn url_set_scheme(url: Url, scheme: &str) -> anyhow::Result<Url> {
	let url = format!(
		"{}:{}",
		scheme,
		url.to_string().split_once(":").context("invalid URL")?.1
	)
	.parse()?;
	Ok(url)
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	#[test]
	fn test_toml_disable_verify_survives_update_from() {
		let toml = r#"
			tls.disable_verify = true
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.tls.disable_verify, Some(true));

		// Simulate: TOML loaded, then CLI args re-applied (no --tls-disable-verify flag).
		config.update_from(["test"]);
		assert_eq!(config.tls.disable_verify, Some(true));
	}

	#[test]
	fn test_cli_disable_verify_flag() {
		let config = ClientConfig::parse_from(["test", "--tls-disable-verify"]);
		assert_eq!(config.tls.disable_verify, Some(true));
	}

	#[test]
	fn test_cli_disable_verify_explicit_false() {
		let config = ClientConfig::parse_from(["test", "--tls-disable-verify", "false"]);
		assert_eq!(config.tls.disable_verify, Some(false));
	}

	#[test]
	fn test_cli_no_disable_verify() {
		let config = ClientConfig::parse_from(["test"]);
		assert_eq!(config.tls.disable_verify, None);
	}
}
