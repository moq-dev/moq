use crate::QuicBackend;
use crate::crypto;
use anyhow::Context;
use std::path::PathBuf;
use std::{net, sync::Arc};
use url::Url;
#[cfg(feature = "iroh")]
use web_transport_iroh::iroh;

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
	#[cfg(not(any(feature = "quinn", feature = "quiche")))]
	pub fn new(_config: ClientConfig) -> anyhow::Result<Self> {
		anyhow::bail!("no QUIC backend compiled; enable quinn or quiche feature");
	}

	/// Create a new client
	#[cfg(any(feature = "quinn", feature = "quiche"))]
	pub fn new(config: ClientConfig) -> anyhow::Result<Self> {
		let backend = config.backend.clone().unwrap_or({
			if cfg!(feature = "quinn") {
				QuicBackend::Quinn
			} else {
				QuicBackend::Quiche
			}
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

		let inner = match backend {
			QuicBackend::Quinn => {
				#[cfg(not(feature = "quinn"))]
				anyhow::bail!("quinn backend not compiled; rebuild with --features quinn");

				#[cfg(feature = "quinn")]
				ClientInner::Quinn(crate::quinn::QuinnClient::new(&config)?)
			}
			QuicBackend::Quiche => {
				#[cfg(not(feature = "quiche"))]
				anyhow::bail!("quiche backend not compiled; rebuild with --features quiche");

				#[cfg(feature = "quiche")]
				ClientInner::Quiche(crate::quiche::QuicheClient::new(&config)?)
			}
		};

		Ok(Self {
			moq: moq_lite::Client::new(),
			#[cfg(feature = "websocket")]
			websocket: config.websocket,
			tls,
			inner,
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

	#[cfg(not(any(feature = "quinn", feature = "quiche", feature = "iroh")))]
	pub async fn connect(&self, _url: Url) -> anyhow::Result<moq_lite::Session> {
		anyhow::bail!("no QUIC backend compiled; enable quinn, quiche, or iroh feature");
	}

	#[cfg(any(feature = "quinn", feature = "quiche", feature = "iroh"))]
	pub async fn connect(&self, url: Url) -> anyhow::Result<moq_lite::Session> {
		#[cfg(feature = "iroh")]
		if crate::iroh::is_iroh_url(&url) {
			let session = self.connect_iroh(url).await?;
			let session = self.moq.connect(session).await?;
			return Ok(session);
		}

		match self.inner {
			#[cfg(feature = "quinn")]
			ClientInner::Quinn(ref quinn) => {
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

					Ok(tokio::select! {
						Ok(quic) = quic_handle => self.moq.connect(quic).await?,
						Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
						else => anyhow::bail!("failed to connect to server"),
					})
				}

				#[cfg(not(feature = "websocket"))]
				{
					let session = quic_handle.await?;
					Ok(self.moq.connect(session).await?)
				}
			}
			#[cfg(feature = "quiche")]
			ClientInner::Quiche(ref quiche) => {
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

					Ok(tokio::select! {
						Ok(quic) = quic_handle => self.moq.connect(quic).await?,
						Some(Ok(ws)) = ws_handle => self.moq.connect(ws).await?,
						else => anyhow::bail!("failed to connect to server"),
					})
				}

				#[cfg(not(feature = "websocket"))]
				{
					let session = quic_handle.await?;
					Ok(self.moq.connect(session).await?)
				}
			}
		}
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
