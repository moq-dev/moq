use crate::Error;
use crate::client::ClientConfig;
use crate::crypto;
use crate::server::{ServerConfig, ServerTlsInfo};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs;
use std::io;
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
	pub versions: moq_net::Versions,
}

impl QuicheClient {
	pub fn new(config: &ClientConfig) -> crate::Result<Self> {
		if !config.tls.root.is_empty() {
			tracing::warn!("--tls-root is not supported with the quiche backend; system roots will be used");
		}

		Ok(Self {
			bind: config.bind,
			disable_verify: config.tls.disable_verify.unwrap_or_default(),
			max_streams: config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS),
			versions: config.versions(),
		})
	}

	pub async fn connect(&self, url: Url) -> crate::Result<web_transport_quiche::Connection> {
		let host = url.host().ok_or(Error::InvalidDnsName)?.to_string();
		let port = url.port().unwrap_or(443);

		if url.scheme() == "http" {
			return Err(Error::QuicheFingerprintUnsupported);
		}

		let alpns: Vec<Vec<u8>> = match url.scheme() {
			"https" => vec![web_transport_quiche::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => self
				.versions
				.alpns()
				.iter()
				.map(|alpn| alpn.as_bytes().to_vec())
				.collect(),
			_ => return Err(Error::InvalidScheme),
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
		for alpn in self.versions.alpns() {
			request = request.with_protocol(alpn.to_string());
		}

		match url.scheme() {
			"https" => {
				// WebTransport over HTTP/3
				let conn = builder
					.connect(&host, port)
					.await
					.map_err(Error::QuicheConnect)?
					.established()
					.await
					.map_err(Error::QuicheEstablish)?;
				let session = web_transport_quiche::Connection::connect(conn, request)
					.await
					.map_err(Error::QuicheClientConnect)?;
				Ok(session)
			}
			"moqt" | "moql" => {
				// Raw QUIC mode
				let conn = builder
					.connect(&host, port)
					.await
					.map_err(Error::QuicheConnect)?
					.established()
					.await
					.map_err(Error::QuicheEstablish)?;

				let alpn = conn.alpn().ok_or(Error::MissingAlpn)?;
				let alpn = std::str::from_utf8(&alpn)?;

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
	pub fn new(config: ServerConfig) -> crate::Result<Self> {
		if config.quic_lb_id.is_some() {
			tracing::warn!("QUIC-LB is not supported with the quiche backend; ignoring server ID");
		}

		let listen = crate::util::resolve(config.bind.as_deref(), crate::server::DEFAULT_BIND)
			.map_err(|e| Error::ResolveBind(Box::new(e)))?;

		let (chain, key) = if !config.tls.generate.is_empty() {
			generate_quiche_cert(&config.tls.generate)?
		} else {
			if config.tls.cert.is_empty() || config.tls.key.is_empty() {
				return Err(Error::QuicheCertRequired);
			}
			if config.tls.cert.len() != config.tls.key.len() {
				return Err(Error::QuicheCertPairMismatch);
			}

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
			#[cfg(any(feature = "noq", feature = "quinn"))]
			certs: Vec::new(),
			fingerprints,
		}));

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = config
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
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
			.map_err(Error::QuicheServerBuild)?;

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

	pub fn local_addr(&self) -> crate::Result<net::SocketAddr> {
		self.server.local_addrs().first().copied().ok_or(Error::NoLocalAddr)
	}

	pub fn close(&mut self) {
		// quiche server doesn't have a close method; dropping it is sufficient
	}
}

fn load_quiche_cert(
	cert_path: &PathBuf,
	key_path: &PathBuf,
) -> crate::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	let chain_file = fs::File::open(cert_path).map_err(Error::OpenCert)?;
	let mut chain_reader = io::BufReader::new(chain_file);

	let chain: Vec<CertificateDer> = CertificateDer::pem_reader_iter(&mut chain_reader)
		.collect::<Result<_, _>>()
		.map_err(Error::ReadCerts)?;

	if chain.is_empty() {
		return Err(Error::NoCerts);
	}

	let key = PrivateKeyDer::from_pem_file(key_path).map_err(Error::KeyPem)?;

	Ok((chain, key))
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
fn generate_quiche_cert(hostnames: &[String]) -> crate::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
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
fn generate_quiche_cert(hostnames: &[String]) -> crate::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	return Err(Error::NoCryptoProvider);
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
		alpns: Vec<&'static str>,
	},
}

impl QuicheRequest {
	pub async fn accept(incoming: web_transport_quiche::ez::Incoming, alpns: Vec<&'static str>) -> crate::Result<Self> {
		tracing::debug!(ip = %incoming.peer_addr(), "accepting via quiche");

		// Accept the connection and wait for it to be established
		let conn = incoming.accept().await?;

		// Get the negotiated ALPN from the established connection
		let alpn = conn.alpn().ok_or(Error::MissingAlpn)?;
		let alpn = std::str::from_utf8(&alpn)?;
		tracing::debug!(ip = %conn.peer_addr(), ?alpn, "accepted via quiche");

		match alpn {
			web_transport_quiche::ALPN => {
				// WebTransport over HTTP/3
				let request = web_transport_quiche::h3::Request::accept(conn)
					.await
					.map_err(Error::QuicheAcceptRequest)?;
				Ok(Self::WebTransport { request, alpns })
			}
			alpn if moq_net::ALPNS.contains(&alpn) => Ok(Self::Raw {
				connection: conn,
				request: ConnectRequest::new("moqt://".to_string().parse::<Url>().unwrap()),
				response: web_transport_quiche::proto::ConnectResponse::OK.with_protocol(alpn),
			}),
			_ => Err(Error::UnsupportedAlpn(alpn.to_string())),
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
			QuicheRequest::WebTransport { request, alpns } => {
				let mut response = web_transport_quiche::proto::ConnectResponse::OK;
				// Pick the first sub-protocol that we actually support.
				// This is the WebTransport equivalent of ALPN negotiation.
				if let Some(protocol) = request.protocols.iter().find(|p| alpns.contains(&p.as_str())) {
					response = response.with_protocol(protocol);
				}
				request.respond(response).await
			}
		}
	}

	/// Returns the URL for this connection.
	pub fn url(&self) -> Option<&Url> {
		match self {
			QuicheRequest::Raw { .. } => None,
			QuicheRequest::WebTransport { request, .. } => Some(&request.url),
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
			QuicheRequest::WebTransport { request, alpns: _, .. } => request.reject(status).await,
		}
	}
}
