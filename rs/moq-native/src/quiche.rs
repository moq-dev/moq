use crate::client::ClientConfig;
use crate::crypto;
use crate::server::{ServerConfig, ServerTlsInfo};
use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs;
use std::io::{self, Cursor, Read};
use std::net;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use url::Url;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuicheClient {
	pub bind: net::SocketAddr,
	pub disable_verify: bool,
}

impl QuicheClient {
	pub fn new(config: &ClientConfig) -> anyhow::Result<Self> {
		if !config.tls.root.is_empty() {
			tracing::warn!("--tls-root is not supported with the quiche backend; system roots will be used");
		}

		Ok(Self {
			bind: config.bind,
			disable_verify: config.tls.disable_verify.unwrap_or_default(),
		})
	}

	pub async fn connect(&self, url: Url) -> anyhow::Result<web_transport_quiche::Connection> {
		let host = url.host().context("invalid DNS name")?.to_string();
		let port = url.port().unwrap_or(443);

		if url.scheme() == "http" {
			anyhow::bail!("fingerprint verification (http:// scheme) is not supported with the quiche backend");
		}

		let alpn = match url.scheme() {
			"https" => web_transport_quiche::ALPN,
			"moql" => moq_lite::lite::ALPN,
			"moqt" => moq_lite::ietf::ALPN,
			_ => anyhow::bail!("url scheme must be 'https' or 'moql'"),
		};

		let mut settings = web_transport_quiche::Settings::default();
		settings.verify_peer = !self.disable_verify;

		let builder = web_transport_quiche::ez::ClientBuilder::default()
			.with_settings(settings)
			.with_bind(self.bind)?;

		tracing::debug!(%url, %alpn, "connecting via quiche");

		match alpn {
			web_transport_quiche::ALPN => {
				// WebTransport over HTTP/3
				let conn = builder
					.connect(&host, port)
					.await
					.map_err(|e| anyhow::anyhow!("quiche connect failed: {e}"))?;
				let session = web_transport_quiche::Connection::connect(conn, url)
					.await
					.map_err(|e| anyhow::anyhow!("WebTransport handshake failed: {e}"))?;
				Ok(session)
			}
			_ => {
				// Raw QUIC mode
				let conn = builder
					.connect(&host, port)
					.await
					.map_err(|e| anyhow::anyhow!("quiche connect failed: {e}"))?;
				Ok(web_transport_quiche::Connection::raw(conn, url))
			}
		}
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct QuicheServer {
	pub server: web_transport_quiche::ez::Server,
	pub fingerprints: Arc<RwLock<ServerTlsInfo>>,
}

impl QuicheServer {
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		if config.quic_lb_id.is_some() {
			tracing::warn!("QUIC-LB is not supported with the quiche backend; ignoring server ID");
		}

		let listen = config.bind.unwrap_or("[::]:443".parse().unwrap());

		let (chain, key) = if !config.tls.generate.is_empty() {
			generate_quiche_cert(&config.tls.generate)?
		} else {
			anyhow::ensure!(
				!config.tls.cert.is_empty() && !config.tls.key.is_empty(),
				"--tls-cert and --tls-key are required with the quiche backend"
			);
			anyhow::ensure!(
				config.tls.cert.len() == config.tls.key.len(),
				"must provide matching --tls-cert and --tls-key pairs"
			);

			// Load certs in PEM format and convert to DER for quiche
			load_quiche_cert(&config.tls.cert[0], &config.tls.key[0])?
		};

		// Compute fingerprints using rustls crypto (always available)
		let provider = crypto::provider();
		let fingerprints: Vec<String> = chain
			.iter()
			.map(|cert| hex::encode(crypto::sha256(&provider, cert.as_ref())))
			.collect();

		let info = Arc::new(RwLock::new(ServerTlsInfo {
			#[cfg(feature = "quinn")]
			certs: Vec::new(),
			fingerprints,
		}));

		let alpn = vec![
			b"h3".to_vec(),
			moq_lite::lite::ALPN.as_bytes().to_vec(),
			moq_lite::ietf::ALPN.as_bytes().to_vec(),
		];

		let server = web_transport_quiche::ez::ServerBuilder::default()
			.with_alpn(alpn)
			.with_bind(listen)?
			.with_single_cert(chain, key)
			.map_err(|e| anyhow::anyhow!("failed to create quiche server: {e}"))?;

		Ok(Self {
			server,
			fingerprints: info,
		})
	}

	pub fn accept(&mut self) -> impl std::future::Future<Output = Option<web_transport_quiche::ez::Incoming>> + '_ {
		self.server.accept()
	}

	pub fn tls_info(&self) -> Arc<RwLock<ServerTlsInfo>> {
		self.fingerprints.clone()
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		self.server
			.local_addrs()
			.first()
			.copied()
			.context("failed to get local address")
	}

	pub fn close(&mut self) {
		// quiche server doesn't have a close method; dropping it is sufficient
	}
}

pub(crate) async fn accept_quiche_session(
	server: moq_lite::Server,
	incoming: web_transport_quiche::ez::Incoming,
) -> anyhow::Result<crate::server::Request> {
	tracing::debug!(ip = %incoming.peer_addr(), "accepting via quiche");

	// Accept the connection and wait for it to be established
	let conn = incoming.accept().await?;

	// Get the negotiated ALPN from the established connection
	let alpn = conn.alpn().context("missing ALPN")?;
	let alpn = std::str::from_utf8(&alpn).context("failed to decode ALPN")?;
	tracing::debug!(ip = %conn.peer_addr(), ?alpn, "accepted via quiche");

	match alpn {
		web_transport_quiche::ALPN => {
			// WebTransport over HTTP/3
			let request = web_transport_quiche::h3::Request::accept(conn)
				.await
				.map_err(|e| anyhow::anyhow!("failed to accept WebTransport request: {e}"))?;
			Ok(crate::server::Request {
				server: server.clone(),
				kind: crate::server::RequestKind::QuicheWebTransport(request),
			})
		}
		_ => {
			// Raw QUIC mode (moql or moqt)
			Ok(crate::server::Request {
				server: server.clone(),
				kind: crate::server::RequestKind::QuicheQuic(QuicheQuicRequest::accept(conn)),
			})
		}
	}
}

fn load_quiche_cert(
	cert_path: &PathBuf,
	key_path: &PathBuf,
) -> anyhow::Result<(Vec<CertificateDer<'static>>, rustls::pki_types::PrivateKeyDer<'static>)> {
	let chain_file = fs::File::open(cert_path).context("failed to open cert file")?;
	let mut chain_reader = io::BufReader::new(chain_file);

	let chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut chain_reader)
		.collect::<Result<_, _>>()
		.context("failed to read certs")?;

	anyhow::ensure!(!chain.is_empty(), "could not find certificate");

	let mut key_buf = Vec::new();
	let mut key_file = fs::File::open(key_path).context("failed to open key file")?;
	key_file.read_to_end(&mut key_buf)?;

	let key = rustls_pemfile::private_key(&mut Cursor::new(&key_buf))?.context("missing private key")?;

	Ok((chain, key))
}

fn generate_quiche_cert(
	hostnames: &[String],
) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	let key_pair = rcgen::KeyPair::generate()?;

	let mut params = rcgen::CertificateParams::new(hostnames)?;

	// Make the certificate valid for two weeks, starting yesterday (in case of clock drift).
	// WebTransport certificates MUST be valid for two weeks at most.
	params.not_before = ::time::OffsetDateTime::now_utc() - ::time::Duration::days(1);
	params.not_after = params.not_before + ::time::Duration::days(14);

	let cert = params.self_signed(&key_pair)?;

	let key_der = key_pair.serialized_der().to_vec();
	let key = PrivateKeyDer::Pkcs8(key_der.into());

	Ok((vec![cert.into()], key))
}

// ── QuicheQuicRequest ───────────────────────────────────────────────

/// A raw QUIC connection request via the quiche backend (not using HTTP/3).
pub struct QuicheQuicRequest {
	connection: web_transport_quiche::ez::Connection,
	url: Url,
}

impl QuicheQuicRequest {
	/// Accept a new raw QUIC session from a client.
	pub fn accept(connection: web_transport_quiche::ez::Connection) -> Self {
		let url: Url = format!("moql://{}", connection.peer_addr())
			.parse()
			.expect("URL is valid");
		Self { connection, url }
	}

	/// Accept the session, wrapping as a raw WebTransport-compatible connection.
	pub fn ok(self) -> web_transport_quiche::Connection {
		web_transport_quiche::Connection::raw(self.connection, self.url)
	}

	/// Returns the URL for this connection.
	#[allow(dead_code)]
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// Reject the session with a status code.
	pub fn close(self, status: web_transport_quiche::http::StatusCode) {
		self.connection.close(status.as_u16().into(), status.as_str());
	}
}
