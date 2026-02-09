use crate::QuicBackend;
use crate::crypto;
use anyhow::Context;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::{net, sync::Arc, time};
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

	/// The QUIC backend to use.
	/// Auto-detected from compiled features if not specified.
	#[arg(long = "quic-backend", env = "MOQ_QUIC_BACKEND")]
	pub backend: Option<QuicBackend>,

	#[command(flatten)]
	#[serde(default)]
	pub tls: ClientTls,

	#[command(flatten)]
	#[serde(default)]
	pub websocket: ClientWebSocket,
}

impl ClientConfig {
	pub fn init(self) -> anyhow::Result<Client> {
		let backend = self.backend.clone().unwrap_or_else(|| {
			if cfg!(feature = "quinn") {
				QuicBackend::Quinn
			} else if cfg!(feature = "quiche") {
				QuicBackend::Quiche
			} else {
				panic!("no QUIC backend compiled; enable quinn or quiche feature")
			}
		});

		let tls = Self::build_tls_config(&self)?;

		let inner = match backend {
			QuicBackend::Quinn => {
				#[cfg(not(feature = "quinn"))]
				anyhow::bail!("quinn backend not compiled; rebuild with --features quinn");

				#[cfg(feature = "quinn")]
				ClientInner::Quinn(crate::quinn::QuinnClient::new(&self)?)
			}
			QuicBackend::Quiche => {
				#[cfg(not(feature = "quiche"))]
				anyhow::bail!("quiche backend not compiled; rebuild with --features quiche");

				#[cfg(feature = "quiche")]
				ClientInner::Quiche(crate::quiche::QuicheClient::new(&self)?)
			}
		};

		Ok(Client {
			moq: moq_lite::Client::new(),
			websocket: self.websocket,
			tls,
			inner,
			#[cfg(feature = "iroh")]
			iroh: None,
		})
	}

	/// Build the rustls ClientConfig used for WebSocket TLS and (with quinn) QUIC TLS.
	fn build_tls_config(config: &ClientConfig) -> anyhow::Result<rustls::ClientConfig> {
		let provider = crypto::provider();

		// Create a list of acceptable root certificates.
		let mut roots = rustls::RootCertStore::empty();

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
				let root = std::fs::File::open(root).context("failed to open root cert file")?;
				let mut root = std::io::BufReader::new(root);

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

		Ok(tls)
	}
}

impl Default for ClientConfig {
	fn default() -> Self {
		Self {
			bind: "[::]:0".parse().unwrap(),
			backend: None,
			tls: ClientTls::default(),
			websocket: ClientWebSocket::default(),
		}
	}
}

/// Client for establishing MoQ connections over QUIC, WebTransport, or WebSocket.
///
/// Create via [`ClientConfig::init`] or [`Client::new`].
#[derive(Clone)]
pub struct Client {
	moq: moq_lite::Client,
	websocket: ClientWebSocket,
	inner: ClientInner,
	tls: rustls::ClientConfig,
	#[cfg(feature = "iroh")]
	iroh: Option<iroh::Endpoint>,
}

#[derive(Clone)]
enum ClientInner {
	#[cfg(feature = "quinn")]
	Quinn(crate::quinn::QuinnClient),
	#[cfg(feature = "quiche")]
	Quiche(crate::quiche::QuicheClient),
}

impl Client {
	/// Create a new client using the default (quinn) backend.
	///
	/// This is equivalent to calling `ClientConfig::default().init()`.
	#[cfg(feature = "quinn")]
	pub fn new(config: ClientConfig) -> anyhow::Result<Self> {
		config.init()
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

	/// Establish a WebTransport/QUIC connection followed by a MoQ handshake.
	pub async fn connect(&self, url: Url) -> anyhow::Result<moq_lite::Session> {
		#[cfg(feature = "iroh")]
		if crate::iroh::is_iroh_url(&url) {
			let session = self.connect_iroh(url).await?;
			let session = self.moq.connect(session).await?;
			return Ok(session);
		}

		match &self.inner {
			#[cfg(feature = "quinn")]
			ClientInner::Quinn(quinn) => {
				let tls = self.tls.clone();
				let quic_url = url.clone();
				let quic_handle = async {
					let res = quinn.connect(&tls, quic_url).await;
					if let Err(err) = &res {
						tracing::warn!(%err, "QUIC connection failed");
					}
					res
				};

				let ws_handle = self.ws_race_handle(url);

				Ok(tokio::select! {
					Ok(quic) = quic_handle => self.moq.connect(quic).await?,
					Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
					else => anyhow::bail!("failed to connect to server"),
				})
			}
			#[cfg(feature = "quiche")]
			ClientInner::Quiche(quiche) => {
				let quic_url = url.clone();
				let quic_handle = async {
					let res = quiche.connect(quic_url).await;
					if let Err(err) = &res {
						tracing::warn!(%err, "QUIC connection failed");
					}
					res
				};

				let ws_handle = self.ws_race_handle(url);

				Ok(tokio::select! {
					Ok(quic) = quic_handle => self.moq.connect(quic).await?,
					Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
					else => anyhow::bail!("failed to connect to server"),
				})
			}
		}
	}

	async fn ws_race_handle(&self, url: Url) -> Option<anyhow::Result<web_transport_ws::Session>> {
		if !self.websocket.enabled {
			return None;
		}
		let res = self.connect_websocket(url).await;
		if let Err(err) = &res {
			tracing::warn!(%err, "WebSocket connection failed");
		}
		Some(res)
	}

	async fn connect_websocket(&self, mut url: Url) -> anyhow::Result<web_transport_ws::Session> {
		anyhow::ensure!(self.websocket.enabled, "WebSocket support is disabled");

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
			"https" | "moql" | "moqt" => {
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
		let alpn = match url.scheme() {
			"moql+iroh" | "iroh" => moq_lite::lite::ALPN,
			"moqt+iroh" => moq_lite::ietf::ALPN,
			"h3+iroh" => web_transport_iroh::ALPN_H3,
			_ => anyhow::bail!("Invalid URL: unknown scheme"),
		};
		let host = url.host().context("Invalid URL: missing host")?.to_string();
		let endpoint_id: iroh::EndpointId = host.parse().context("Invalid URL: host is not an iroh endpoint id")?;
		let conn = endpoint.connect(endpoint_id, alpn.as_bytes()).await?;
		let session = match alpn {
			web_transport_iroh::ALPN_H3 => {
				let url = url_set_scheme(url, "https")?;
				web_transport_iroh::Session::connect_h3(conn, url).await?
			}
			_ => web_transport_iroh::Session::raw(conn),
		};
		Ok(session)
	}
}

use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

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
