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
use web_transport_quiche::proto::ConnectRequest;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuicheClient {
	pub bind: net::SocketAddr,
	pub disable_verify: bool,
	pub max_streams: u64,
}

impl QuicheClient {
	pub fn new(config: &ClientConfig) -> anyhow::Result<Self> {
		if !config.tls.root.is_empty() {
			tracing::warn!("--tls-root is not supported with the quiche backend; system roots will be used");
		}

		Ok(Self {
			bind: config.bind,
			disable_verify: config.tls.disable_verify.unwrap_or_default(),
			max_streams: config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS),
		})
	}

	pub async fn connect(&self, url: Url) -> anyhow::Result<web_transport_quiche::Connection> {
		let host = url.host().context("invalid DNS name")?.to_string();
		let port = url.port().unwrap_or(443);

		if url.scheme() == "http" {
			anyhow::bail!("fingerprint verification (http:// scheme) is not supported with the quiche backend");
		}

		let alpns = match url.scheme() {
			"https" => vec![web_transport_quiche::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => moq_lite::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect(),
			_ => anyhow::bail!("url scheme must be 'https', 'moqt', or 'moql'"),
		};

		let mut settings = web_transport_quiche::Settings::default();
		settings.verify_peer = !self.disable_verify;
		settings.alpn = alpns;
		settings.initial_max_streams_bidi = self.max_streams;
		settings.initial_max_streams_uni = self.max_streams;

		let builder = web_transport_quiche::ez::ClientBuilder::default()
			.with_settings(settings)
			.with_bind(self.bind)?;

		tracing::debug!(%url, "connecting via quiche");

		let mut request = web_transport_quiche::proto::ConnectRequest::new(url.clone());
		for alpn in moq_lite::ALPNS {
			request = request.with_protocol(alpn.to_string());
		}

		match url.scheme() {
			"https" => {
				// WebTransport over HTTP/3
				let conn = builder
					.connect(&host, port)
					.await
					.context("failed to connect to quiche server")?;
				let session = web_transport_quiche::Connection::connect(conn, request)
					.await
					.context("failed to connect to quiche server")?;
				Ok(session)
			}
			"moqt" | "moql" => {
				// Raw QUIC mode
				let conn = builder
					.connect(&host, port)
					.await
					.context("failed to connect to quiche server")?;

				let alpn = conn.alpn().context("missing ALPN")?;
				let alpn = std::str::from_utf8(&alpn).context("failed to decode ALPN")?;

				let response = web_transport_quiche::proto::ConnectResponse::OK.with_protocol(alpn);
				Ok(web_transport_quiche::Connection::raw(conn, request, response))
			}
			_ => unreachable!("unsupported URL scheme: {}", url.scheme()),
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

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = moq_lite::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect();
		alpns.push(b"h3".to_vec());

		let max_streams = config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS);

		let mut settings = web_transport_quiche::Settings::default();
		settings.alpn = alpns;
		settings.initial_max_streams_bidi = max_streams;
		settings.initial_max_streams_uni = max_streams;

		let server = web_transport_quiche::ez::ServerBuilder::default()
			.with_settings(settings)
			.with_bind(listen)?
			.with_single_cert(chain, key)
			.context("failed to create quiche server")?;

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

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
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

#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
fn generate_quiche_cert(
	hostnames: &[String],
) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	anyhow::bail!("no crypto provider available; enable aws-lc-rs or ring feature");
}

// ── QuicheQuicRequest ───────────────────────────────────────────────

/// A raw QUIC connection request via the quiche backend (not using HTTP/3).
pub(crate) enum QuicheRequest {
	Raw {
		connection: web_transport_quiche::ez::Connection,
		request: web_transport_quiche::proto::ConnectRequest,
		response: web_transport_quiche::proto::ConnectResponse,
	},
	WebTransport {
		request: web_transport_quiche::h3::Request,
	},
}

impl QuicheRequest {
	pub async fn accept(incoming: web_transport_quiche::ez::Incoming) -> anyhow::Result<Self> {
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
					.context("failed to accept WebTransport request")?;
				Ok(Self::WebTransport { request })
			}
			alpn if moq_lite::ALPNS.contains(&alpn) => Ok(Self::Raw {
				connection: conn,
				request: ConnectRequest::new("moqt://".to_string().parse::<Url>().unwrap()),
				response: web_transport_quiche::proto::ConnectResponse::OK.with_protocol(alpn),
			}),
			_ => {
				anyhow::bail!("unsupported ALPN: {alpn}")
			}
		}
	}
	/// Accept the session, wrapping as a raw WebTransport-compatible connection.
	pub async fn ok(self) -> Result<web_transport_quiche::Connection, web_transport_quiche::ServerError> {
		match self {
			QuicheRequest::Raw {
				connection,
				request,
				response,
			} => Ok(web_transport_quiche::Connection::raw(connection, request, response)),
			QuicheRequest::WebTransport { request } => {
				let mut response = web_transport_quiche::proto::ConnectResponse::OK;

				// TODO actually pick a valid moq protocol
				if let Some(alpn) = request.protocols.first() {
					response = response.with_protocol(alpn);
				}

				request.respond(response).await
			}
		}
	}

	/// Returns the URL for this connection.
	pub fn url(&self) -> Option<&Url> {
		match self {
			QuicheRequest::Raw { .. } => None,
			QuicheRequest::WebTransport { request } => Some(&request.url),
		}
	}

	/// Reject the session with a status code.
	pub async fn reject(
		self,
		status: web_transport_quiche::http::StatusCode,
	) -> Result<(), web_transport_quiche::ServerError> {
		match self {
			QuicheRequest::Raw { connection, .. } => {
				let _: () = connection.close(status.as_u16().into(), status.as_str());
				Ok(())
			}
			QuicheRequest::WebTransport { request } => request.reject(status).await,
		}
	}
}
