use crate::client::ClientConfig;
use crate::crypto;
use crate::server::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net;
use std::path::Path;
use std::sync::{Arc, RwLock};
use url::Url;
use web_transport_quiche::proto::ConnectRequest;

pub use web_transport_quiche;

/// Errors specific to the quiche QUIC backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error("invalid DNS name")]
	InvalidDnsName,

	/// No longer returned: the `http://` scheme now fetches and pins the
	/// certificate fingerprint instead of failing.
	#[deprecated(note = "fingerprint verification over http:// is now supported; this is never returned")]
	#[error("fingerprint verification (http:// scheme) is not supported with the quiche backend")]
	FingerprintUnsupported,

	#[error("failed to fetch certificate fingerprint")]
	FetchFingerprint(#[source] reqwest::Error),

	#[error("certificate fingerprint request failed")]
	FingerprintStatus(#[source] reqwest::Error),

	#[error("failed to read certificate fingerprint")]
	ReadFingerprint(#[source] reqwest::Error),

	#[error("invalid certificate fingerprint")]
	InvalidFingerprint(#[source] hex::FromHexError),

	#[error("certificate fingerprint must be 32 bytes (SHA-256), got {0}")]
	FingerprintLength(usize),

	#[error("url scheme must be 'https', 'moqt', or 'moql'")]
	InvalidScheme,

	#[error("missing ALPN")]
	MissingAlpn,

	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::str::Utf8Error),

	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	#[error("failed to resolve bind address")]
	ResolveBind(#[source] std::io::Error),

	#[error("failed to get local address")]
	NoLocalAddr,

	#[error("--tls-cert and --tls-key are required with the quiche backend")]
	CertRequired,

	#[error("must provide matching --tls-cert and --tls-key pairs")]
	CertPairMismatch,

	#[error("failed to connect to quiche server")]
	Connect(#[source] std::io::Error),

	#[error(transparent)]
	Connection(#[from] web_transport_quiche::ez::ConnectionError),

	#[error("failed to establish quiche connection")]
	Establish(#[source] web_transport_quiche::ez::ConnectionError),

	#[error("failed to connect to quiche server")]
	ClientConnect(#[from] web_transport_quiche::ClientError),

	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	#[error("failed to create quiche server")]
	ServerBuild(#[source] std::io::Error),

	#[error("failed to accept WebTransport request")]
	AcceptRequest(#[source] web_transport_quiche::ServerError),

	#[error("failed to accept quiche WebTransport")]
	Accept(#[source] web_transport_quiche::ServerError),

	#[error("failed to close quiche WebTransport request")]
	Reject(#[source] web_transport_quiche::ServerError),

	#[error(transparent)]
	Tls(#[from] crate::tls::Error),
}

type Result<T> = std::result::Result<T, Error>;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuicheClient {
	pub bind: net::SocketAddr,
	/// Resolved server-verification policy, shared with the other backends.
	pub verification: crate::tls::Verification,
	/// Whether an `http://` URL may bootstrap a pin (see [crate::tls::Client::allows_http_bootstrap]).
	pub http_bootstrap: bool,
	pub max_streams: u64,
	pub versions: moq_net::Versions,
}

impl QuicheClient {
	pub fn new(config: &ClientConfig) -> Result<Self> {
		Ok(Self {
			bind: config.bind,
			verification: config.tls.verification()?,
			http_bootstrap: config.tls.allows_http_bootstrap(),
			max_streams: config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS),
			versions: config.versions(),
		})
	}

	pub async fn connect(&self, url: Url) -> Result<web_transport_quiche::Connection> {
		use crate::tls::Verification;

		let host = url.host().ok_or(Error::InvalidDnsName)?.to_string();
		let port = url.port().unwrap_or(443);

		// `http://` fetches the relay's self-signed certificate fingerprint over
		// an insecure request and pins it for this connection. It is only honored
		// when no stronger verification is configured: an attacker who controls
		// the plaintext fetch must not be able to weaken an explicit pin or
		// re-enable verification we were told to skip.
		let (url, verification) = if url.scheme() == "http" {
			let mut https = url.clone();
			https.set_scheme("https").expect("https is a valid scheme");

			if self.http_bootstrap {
				let pin = fetch_fingerprint(&url).await?;
				(https, Verification::Fingerprints(vec![pin]))
			} else {
				tracing::warn!(
					"ignoring insecure http:// fingerprint bootstrap; using the configured TLS verification"
				);
				(https, self.verification.clone())
			}
		} else {
			(url, self.verification.clone())
		};

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
		settings.verify_peer = !matches!(verification, Verification::Disabled);
		settings.alpn = alpns;
		settings.initial_max_streams_bidi = self.max_streams;
		settings.initial_max_streams_uni = self.max_streams;

		let mut builder = web_transport_quiche::ez::ClientBuilder::default()
			.with_settings(settings)
			.with_bind(self.bind)?;

		match verification {
			// No hook: tokio-quiche's default config with verify_peer = false.
			Verification::Disabled => {}
			Verification::Fingerprints(hashes) => {
				builder = builder.with_server_certificate_hashes(hashes);
			}
			Verification::Roots { custom, system } => {
				// quiche/boringssl takes a concrete root list rather than a rustls
				// verifier, so the platform verifier the other backends use isn't
				// available here. Trust the native store for system roots; on
				// platforms without one (iOS/Android) this yields nothing and the
				// handshake fails closed.
				let mut roots = custom;
				if system {
					let native = rustls_native_certs::load_native_certs();
					for err in native.errors {
						tracing::warn!(%err, "failed to load native root cert");
					}
					roots.extend(native.certs);
				}
				if !roots.is_empty() {
					builder = builder.with_root_certificates(roots);
				}
			}
		}

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
					.map_err(Error::Connect)?
					.established()
					.await
					.map_err(Error::Establish)?;
				let session = web_transport_quiche::Connection::connect(conn, request)
					.await
					.map_err(map_client_error)?;
				Ok(session)
			}
			"moqt" | "moql" => {
				// Raw QUIC mode
				let conn = builder
					.connect(&host, port)
					.await
					.map_err(Error::Connect)?
					.established()
					.await
					.map_err(Error::Establish)?;

				let alpn = conn.alpn().ok_or(Error::MissingAlpn)?;
				let alpn = std::str::from_utf8(&alpn)?;

				let response = web_transport_quiche::proto::ConnectResponse::OK.with_protocol(alpn);
				Ok(web_transport_quiche::Connection::raw(conn, request, response))
			}
			_ => unreachable!("unsupported URL scheme: {}", url.scheme()),
		}
	}
}

/// Fetch a relay's certificate SHA-256 over an insecure `http://` request.
///
/// This is the native equivalent of how a browser bootstraps trust for a
/// self-signed relay: GET `/certificate.sha256` and pin the returned hash.
async fn fetch_fingerprint(url: &Url) -> Result<[u8; 32]> {
	let mut fp = url.clone();
	fp.set_path("/certificate.sha256");
	fp.set_query(None);
	fp.set_fragment(None);

	tracing::warn!(url = %fp, "performing insecure HTTP request for certificate fingerprint");

	let resp = reqwest::get(fp.as_str())
		.await
		.map_err(Error::FetchFingerprint)?
		.error_for_status()
		.map_err(Error::FingerprintStatus)?;
	let text = resp.text().await.map_err(Error::ReadFingerprint)?;
	let bytes = hex::decode(text.trim()).map_err(Error::InvalidFingerprint)?;
	bytes.try_into().map_err(|v: Vec<u8>| Error::FingerprintLength(v.len()))
}

impl Error {
	pub(crate) fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::ConnectRejected(err) => Some(*err),
			Self::ClientConnect(err) => classify_client_error(err),
			_ => None,
		}
	}
}

fn map_client_error(err: web_transport_quiche::ClientError) -> Error {
	if let Some(err) = classify_client_error(&err) {
		return err.into();
	}

	err.into()
}

fn classify_client_error(err: &web_transport_quiche::ClientError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quiche::ClientError::Connect(err) => classify_connect_error(err),
		_ => None,
	}
}

fn classify_connect_error(err: &web_transport_quiche::h3::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quiche::h3::ConnectError::Status(status) => crate::ConnectError::from_status_u16(status.as_u16()),
		web_transport_quiche::h3::ConnectError::Proto(err) => classify_proto_error(err),
		_ => None,
	}
}

fn classify_proto_error(err: &web_transport_quiche::proto::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quiche::proto::ConnectError::ErrorStatus(status)
		| web_transport_quiche::proto::ConnectError::WrongStatus(Some(status)) => {
			crate::ConnectError::from_status_u16(status.as_u16())
		}
		_ => None,
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct QuicheServer {
	pub server: web_transport_quiche::ez::Server,
	pub fingerprints: Arc<RwLock<crate::tls::Info>>,
}

impl QuicheServer {
	pub fn new(config: ServerConfig) -> Result<Self> {
		if config.quic_lb_id.is_some() {
			tracing::warn!("QUIC-LB is not supported with the quiche backend; ignoring server ID");
		}

		let listen =
			crate::util::resolve(config.bind.as_deref(), crate::server::DEFAULT_BIND).map_err(Error::ResolveBind)?;

		let (chain, key) = if !config.tls.generate.is_empty() {
			generate_quiche_cert(&config.tls.generate)?
		} else {
			if config.tls.cert.is_empty() || config.tls.key.is_empty() {
				return Err(Error::CertRequired);
			}
			if config.tls.cert.len() != config.tls.key.len() {
				return Err(Error::CertPairMismatch);
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

		let info = Arc::new(RwLock::new(crate::tls::Info {
			#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
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
			.map_err(Error::ServerBuild)?;

		Ok(Self {
			server,
			fingerprints: info,
		})
	}

	pub fn accept(&mut self) -> impl std::future::Future<Output = Option<web_transport_quiche::ez::Incoming>> + '_ {
		self.server.accept()
	}

	pub fn tls_info(&self) -> Arc<RwLock<crate::tls::Info>> {
		self.fingerprints.clone()
	}

	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		self.server.local_addrs().first().copied().ok_or(Error::NoLocalAddr)
	}

	pub fn close(&mut self) {
		// quiche server doesn't have a close method; dropping it is sufficient
	}
}

fn load_quiche_cert(
	cert_path: &Path,
	key_path: &Path,
) -> crate::tls::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	let chain = crate::tls::read_certs(cert_path)?;
	if chain.is_empty() {
		return Err(crate::tls::Error::Empty);
	}

	let key = PrivateKeyDer::from_pem_file(key_path).map_err(crate::tls::Error::Key)?;

	Ok((chain, key))
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
fn generate_quiche_cert(
	hostnames: &[String],
) -> crate::tls::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
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
) -> crate::tls::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	Err(crate::tls::Error::NoCryptoProvider)
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
	pub async fn accept(incoming: web_transport_quiche::ez::Incoming, alpns: Vec<&'static str>) -> Result<Self> {
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
					.map_err(Error::AcceptRequest)?;
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
	pub async fn ok(self) -> std::result::Result<web_transport_quiche::Connection, web_transport_quiche::ServerError> {
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
	) -> std::result::Result<(), web_transport_quiche::ServerError> {
		match self {
			QuicheRequest::Raw { connection, .. } => {
				let _: () = connection.close(status.as_u16().into(), status.as_str());
				Ok(())
			}
			QuicheRequest::WebTransport { request, alpns: _, .. } => request.reject(status).await,
		}
	}
}
