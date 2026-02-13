use crate::QuicBackend;
use crate::crypto;
use anyhow::Context;
use std::path::PathBuf;
use std::{net, sync::Arc};
use url::Url;

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
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub disable_verify: Option<bool>,
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
	#[arg(id = "client-backend", long = "client-backend", env = "MOQ_CLIENT_BACKEND")]
	pub backend: Option<QuicBackend>,

	/// Maximum number of concurrent QUIC streams per connection (both bidi and uni).
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "client-max-streams",
		long = "client-max-streams",
		env = "MOQ_CLIENT_MAX_STREAMS"
	)]
	pub max_streams: Option<u64>,

	#[command(flatten)]
	#[serde(default)]
	pub tls: ClientTls,

	#[cfg(feature = "websocket")]
	#[command(flatten)]
	#[serde(default)]
	pub websocket: super::ClientWebSocket,
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
			backend: None,
			max_streams: None,
			tls: ClientTls::default(),
			#[cfg(feature = "websocket")]
			websocket: super::ClientWebSocket::default(),
		}
	}
}

/// Client for establishing MoQ connections over QUIC, WebTransport, or WebSocket.
///
/// Create via [`ClientConfig::init`] or [`Client::new`].
#[derive(Clone)]
pub struct Client {
	moq: moq_lite::Client,
	#[cfg(feature = "websocket")]
	websocket: super::ClientWebSocket,
	tls: rustls::ClientConfig,
	#[cfg(feature = "quinn")]
	quinn: Option<crate::quinn::QuinnClient>,
	#[cfg(feature = "quiche")]
	quiche: Option<crate::quiche::QuicheClient>,
	#[cfg(feature = "iroh")]
	iroh: Option<web_transport_iroh::iroh::Endpoint>,
}

impl Client {
	#[cfg(not(any(feature = "quinn", feature = "quiche")))]
	pub fn new(_config: ClientConfig) -> anyhow::Result<Self> {
		anyhow::bail!("no QUIC backend compiled; enable quinn or quiche feature");
	}

	/// Create a new client
	#[cfg(any(feature = "quinn", feature = "quiche"))]
	pub fn new(config: ClientConfig) -> anyhow::Result<Self> {
		let backend = config.backend.clone().unwrap_or({
			#[cfg(feature = "quinn")]
			{
				QuicBackend::Quinn
			}
			#[cfg(all(feature = "quiche", not(feature = "quinn")))]
			{
				QuicBackend::Quiche
			}
			#[cfg(all(not(feature = "quiche"), not(feature = "quinn")))]
			panic!("no QUIC backend compiled; enable quinn or quiche feature");
		});

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

		#[cfg(feature = "quinn")]
		#[allow(unreachable_patterns)]
		let quinn = match backend {
			QuicBackend::Quinn => Some(crate::quinn::QuinnClient::new(&config)?),
			_ => None,
		};

		#[cfg(feature = "quiche")]
		let quiche = match backend {
			QuicBackend::Quiche => Some(crate::quiche::QuicheClient::new(&config)?),
			_ => None,
		};

		Ok(Self {
			moq: moq_lite::Client::new(),
			#[cfg(feature = "websocket")]
			websocket: config.websocket,
			tls,
			#[cfg(feature = "quinn")]
			quinn,
			#[cfg(feature = "quiche")]
			quiche,
			#[cfg(feature = "iroh")]
			iroh: None,
		})
	}

	#[cfg(feature = "iroh")]
	pub fn with_iroh(mut self, iroh: Option<web_transport_iroh::iroh::Endpoint>) -> Self {
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

	#[cfg(not(any(feature = "quinn", feature = "quiche", feature = "iroh")))]
	pub async fn connect(&self, _url: Url) -> anyhow::Result<moq_lite::Session> {
		anyhow::bail!("no QUIC backend compiled; enable quinn, quiche, or iroh feature");
	}

	#[cfg(any(feature = "quinn", feature = "quiche", feature = "iroh"))]
	pub async fn connect(&self, url: Url) -> anyhow::Result<moq_lite::Session> {
		#[cfg(feature = "iroh")]
		if crate::iroh::is_iroh_url(&url) {
			let endpoint = self.iroh.as_ref().context("Iroh support is not enabled")?;
			let session = crate::iroh::connect(endpoint, url).await?;
			let session = self.moq.connect(session).await?;
			return Ok(session);
		}

		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_ref() {
			let tls = self.tls.clone();
			let quic_url = url.clone();
			let quic_handle = async {
				let res = quinn.connect(&tls, quic_url).await;
				if let Err(err) = &res {
					tracing::warn!(%err, "QUIC connection failed");
				}
				res
			};

			#[cfg(feature = "websocket")]
			{
				let ws_handle = crate::websocket::race_handle(&self.websocket, &self.tls, url);

				return Ok(tokio::select! {
					Ok(quic) = quic_handle => self.moq.connect(quic).await?,
					Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
					else => anyhow::bail!("failed to connect to server"),
				});
			}

			#[cfg(not(feature = "websocket"))]
			{
				let session = quic_handle.await?;
				return Ok(self.moq.connect(session).await?);
			}
		}

		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_ref() {
			let quic_url = url.clone();
			let quic_handle = async {
				let res = quiche.connect(quic_url).await;
				if let Err(err) = &res {
					tracing::warn!(%err, "QUIC connection failed");
				}
				res
			};

			#[cfg(feature = "websocket")]
			{
				let ws_handle = crate::websocket::race_handle(&self.websocket, &self.tls, url);

				return Ok(tokio::select! {
					Ok(quic) = quic_handle => self.moq.connect(quic).await?,
					Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
					else => anyhow::bail!("failed to connect to server"),
				});
			}

			#[cfg(not(feature = "websocket"))]
			{
				let session = quic_handle.await?;
				return Ok(self.moq.connect(session).await?);
			}
		}

		anyhow::bail!("no QUIC backend compiled; enable quinn or quiche feature");
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
		let config = ClientConfig::parse_from(["test", "--tls-disable-verify=false"]);
		assert_eq!(config.tls.disable_verify, Some(false));
	}

	#[test]
	fn test_cli_disable_verify_explicit_true() {
		let config = ClientConfig::parse_from(["test", "--tls-disable-verify=true"]);
		assert_eq!(config.tls.disable_verify, Some(true));
	}

	#[test]
	fn test_cli_no_disable_verify() {
		let config = ClientConfig::parse_from(["test"]);
		assert_eq!(config.tls.disable_verify, None);
	}
}
