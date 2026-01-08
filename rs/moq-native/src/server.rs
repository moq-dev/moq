use std::path::PathBuf;
use std::{net, time::Duration};

use crate::crypto;
use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::fs;
use std::io::{self, Cursor, Read};
use std::sync::{Arc, RwLock};
use url::Url;
use web_transport_quinn::{http, ServerError};

use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::FutureExt;

#[derive(clap::Args, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerTlsCert {
	pub chain: PathBuf,
	pub key: PathBuf,
}

impl ServerTlsCert {
	// A crude colon separated string parser just for clap support.
	pub fn parse(s: &str) -> anyhow::Result<Self> {
		let (chain, key) = s.split_once(':').context("invalid certificate")?;
		Ok(Self {
			chain: PathBuf::from(chain),
			key: PathBuf::from(key),
		})
	}
}

#[derive(clap::Args, Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerTlsConfig {
	/// Load the given certificate from disk.
	#[arg(long = "tls-cert", id = "tls-cert", env = "MOQ_SERVER_TLS_CERT")]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub cert: Vec<PathBuf>,

	/// Load the given key from disk.
	#[arg(long = "tls-key", id = "tls-key", env = "MOQ_SERVER_TLS_KEY")]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub key: Vec<PathBuf>,

	/// Or generate a new certificate and key with the given hostnames.
	/// This won't be valid unless the client uses the fingerprint or disables verification.
	#[arg(
		long = "tls-generate",
		id = "tls-generate",
		value_delimiter = ',',
		env = "MOQ_SERVER_TLS_GENERATE"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub generate: Vec<String>,
}

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
	/// Listen for UDP packets on the given address.
	/// Defaults to `[::]:443` if not provided.
	#[serde(alias = "listen")]
	#[arg(id = "server-bind", long = "server-bind", alias = "listen", env = "MOQ_SERVER_BIND")]
	pub bind: Option<net::SocketAddr>,

	#[command(flatten)]
	#[serde(default)]
	pub tls: ServerTlsConfig,
}

impl ServerConfig {
	pub fn init(self) -> anyhow::Result<Server> {
		Server::new(self)
	}
}

pub struct Server {
	quic: quinn::Endpoint,
	accept: FuturesUnordered<BoxFuture<'static, anyhow::Result<Request>>>,
	certs: Arc<ServeCerts>,
}

impl Server {
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		// Enable BBR congestion control
		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
		transport.keep_alive_interval(Some(Duration::from_secs(4)));
		//transport.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
		transport.mtu_discovery_config(None); // Disable MTU discovery
		let transport = Arc::new(transport);

		let provider = crypto::provider();

		let certs = ServeCerts::new(provider.clone());

		certs.load_certs(&config.tls)?;

		let certs = Arc::new(certs);

		#[cfg(unix)]
		tokio::spawn(Self::reload_certs(certs.clone(), config.tls.clone()));

		let mut tls = rustls::ServerConfig::builder_with_provider(provider)
			.with_protocol_versions(&[&rustls::version::TLS13])?
			.with_no_client_auth()
			.with_cert_resolver(certs.clone());

		tls.alpn_protocols = vec![
			web_transport_quinn::ALPN.as_bytes().to_vec(),
			moq_lite::lite::ALPN.as_bytes().to_vec(),
			moq_lite::ietf::ALPN.as_bytes().to_vec(),
		];
		tls.key_log = Arc::new(rustls::KeyLogFile::new());

		let tls: quinn::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut tls = quinn::ServerConfig::with_crypto(Arc::new(tls));
		tls.transport_config(transport.clone());

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().context("no async runtime")?;
		let endpoint_config = quinn::EndpointConfig::default();

		let listen = config.bind.unwrap_or("[::]:443".parse().unwrap());
		let socket = std::net::UdpSocket::bind(listen).context("failed to bind UDP socket")?;

		// Create the generic QUIC endpoint.
		let quic = quinn::Endpoint::new(endpoint_config, Some(tls), socket, runtime)
			.context("failed to create QUIC endpoint")?;

		Ok(Self {
			quic: quic.clone(),
			accept: Default::default(),
			certs,
		})
	}

	#[cfg(unix)]
	async fn reload_certs(certs: Arc<ServeCerts>, tls_config: ServerTlsConfig) {
		use tokio::signal::unix::{signal, SignalKind};

		// Dunno why we wouldn't be allowed to listen for signals, but just in case.
		let mut listener = signal(SignalKind::user_defined1()).expect("failed to listen for signals");

		while listener.recv().await.is_some() {
			tracing::info!("reloading server certificates");

			if let Err(err) = certs.load_certs(&tls_config) {
				tracing::warn!(%err, "failed to reload server certificates");
			}
		}
	}

	// Return the SHA256 fingerprints of all our certificates.
	pub fn tls_info(&self) -> Arc<RwLock<TlsInfo>> {
		self.certs.info.clone()
	}

	/// Returns the next partially established QUIC or WebTransport session.
	///
	/// This returns a [Request] instead of a [web_transport_quinn::Session]
	/// so the connection can be rejected early on an invalid path or missing auth.
	///
	/// The [Request] is either a WebTransport or a raw QUIC request.
	/// Call [Request::ok] or [Request::close] to complete the handshake in case this is
	/// a WebTransport request.
	pub async fn accept(&mut self) -> Option<Request> {
		loop {
			tokio::select! {
				res = self.quic.accept() => {
					let conn = res?;
					self.accept.push(Self::accept_session(conn).boxed());
				}
				Some(res) = self.accept.next() => {
					match res {
						Ok(session) => return Some(session),
						Err(err) => tracing::debug!(%err, "failed to accept session"),
					}
				}
				_ = tokio::signal::ctrl_c() => {
					self.close();
					// Give it a chance to close.
					tokio::time::sleep(Duration::from_millis(100)).await;

					return None;
				}
			}
		}
	}

	async fn accept_session(conn: quinn::Incoming) -> anyhow::Result<Request> {
		let mut conn = conn.accept()?;

		let handshake = conn
			.handshake_data()
			.await?
			.downcast::<quinn::crypto::rustls::HandshakeData>()
			.unwrap();

		let alpn = handshake.protocol.context("missing ALPN")?;
		let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;
		let host = handshake.server_name.unwrap_or_default();

		tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepting");

		// Wait for the QUIC connection to be established.
		let conn = conn.await.context("failed to establish QUIC connection")?;

		let span = tracing::Span::current();
		span.record("id", conn.stable_id()); // TODO can we get this earlier?
		tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepted");

		match alpn.as_str() {
			web_transport_quinn::ALPN => {
				// Wait for the CONNECT request.
				let request = web_transport_quinn::Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Request::WebTransport(request))
			}
			moq_lite::lite::ALPN | moq_lite::ietf::ALPN => Ok(Request::Quic(QuicRequest::accept(conn))),
			_ => anyhow::bail!("unsupported ALPN: {alpn}"),
		}
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		self.quic.local_addr().context("failed to get local address")
	}

	pub fn close(&mut self) {
		self.quic.close(quinn::VarInt::from_u32(0), b"server shutdown");
	}
}

pub enum Request {
	WebTransport(web_transport_quinn::Request),
	Quic(QuicRequest),
}

impl Request {
	/// Reject the session, returning your favorite HTTP status code.
	pub async fn reject(self, status: http::StatusCode) -> Result<(), ServerError> {
		match self {
			Self::WebTransport(request) => request.close(status).await,
			Self::Quic(request) => {
				request.close(status);
				Ok(())
			}
		}
	}

	/// Accept the session, performing rest of the MoQ handshake.
	pub async fn accept(
		self,
		publish: impl Into<Option<moq_lite::OriginConsumer>>,
		subscribe: impl Into<Option<moq_lite::OriginProducer>>,
	) -> anyhow::Result<moq_lite::Session> {
		let session = match self {
			Request::WebTransport(request) => request.ok().await?,
			Request::Quic(request) => request.ok(),
		};
		let session = moq_lite::Session::accept(session, publish, subscribe).await?;
		Ok(session)
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> &Url {
		match self {
			Request::WebTransport(request) => request.url(),
			Request::Quic(request) => request.url(),
		}
	}
}

pub struct QuicRequest {
	connection: quinn::Connection,
	url: Url,
}

impl QuicRequest {
	/// Accept a new QUIC session from a client.
	pub fn accept(connection: quinn::Connection) -> Self {
		let url: Url = format!("moql://{}", connection.remote_address())
			.parse()
			.expect("URL is valid");
		Self { connection, url }
	}

	/// Accept the session, returning a 200 OK if using WebTransport.
	pub fn ok(self) -> web_transport_quinn::Session {
		web_transport_quinn::Session::raw(self.connection, self.url)
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// Reject the session with a status code.
	///
	/// The status code number will be used as the error code.
	pub fn close(self, status: http::StatusCode) {
		self.connection
			.close(status.as_u16().into(), status.as_str().as_bytes());
	}
}

#[derive(Debug)]
pub struct TlsInfo {
	pub(crate) certs: Vec<Arc<CertifiedKey>>,
	pub fingerprints: Vec<String>,
}

#[derive(Debug)]
struct ServeCerts {
	info: Arc<RwLock<TlsInfo>>,
	provider: crypto::Provider,
}

impl ServeCerts {
	pub fn new(provider: crypto::Provider) -> Self {
		Self {
			info: Arc::new(RwLock::new(TlsInfo {
				certs: Vec::new(),
				fingerprints: Vec::new(),
			})),
			provider,
		}
	}

	pub fn load_certs(&self, config: &ServerTlsConfig) -> anyhow::Result<()> {
		anyhow::ensure!(config.cert.len() == config.key.len(), "must provide both cert and key");

		let mut certs = Vec::new();

		// Load the certificate and key files based on their index.
		for (cert, key) in config.cert.iter().zip(config.key.iter()) {
			certs.push(Arc::new(self.load(cert, key)?));
		}

		// Generate a new certificate if requested.
		if !config.generate.is_empty() {
			certs.push(Arc::new(self.generate(&config.generate)?));
		}

		self.set_certs(certs);
		Ok(())
	}

	// Load a certificate and corresponding key from a file, but don't add it to the certs
	fn load(&self, chain_path: &PathBuf, key_path: &PathBuf) -> anyhow::Result<CertifiedKey> {
		let chain = fs::File::open(chain_path).context("failed to open cert file")?;
		let mut chain = io::BufReader::new(chain);

		let chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut chain)
			.collect::<Result<_, _>>()
			.context("failed to read certs")?;

		anyhow::ensure!(!chain.is_empty(), "could not find certificate");

		// Read the PEM private key
		let mut keys = fs::File::open(key_path).context("failed to open key file")?;

		// Read the keys into a Vec so we can parse it twice.
		let mut buf = Vec::new();
		keys.read_to_end(&mut buf)?;

		let key = rustls_pemfile::private_key(&mut Cursor::new(&buf))?.context("missing private key")?;
		let key = self.provider.key_provider.load_private_key(key)?;

		let certified_key = CertifiedKey::new(chain, key);

		certified_key.keys_match().context(format!(
			"private key {} doesn't match certificate {}",
			key_path.display(),
			chain_path.display()
		))?;

		Ok(certified_key)
	}

	fn generate(&self, hostnames: &[String]) -> anyhow::Result<CertifiedKey> {
		let key_pair = rcgen::KeyPair::generate()?;

		let mut params = rcgen::CertificateParams::new(hostnames)?;

		// Make the certificate valid for two weeks, starting yesterday (in case of clock drift).
		// WebTransport certificates MUST be valid for two weeks at most.
		params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
		params.not_after = params.not_before + time::Duration::days(14);

		// Generate the certificate
		let cert = params.self_signed(&key_pair)?;

		// Convert the rcgen type to the rustls type.
		let key_der = key_pair.serialized_der().to_vec();
		let key_der = PrivatePkcs8KeyDer::from(key_der);
		let key = self.provider.key_provider.load_private_key(key_der.into())?;

		// Create a rustls::sign::CertifiedKey
		Ok(CertifiedKey::new(vec![cert.into()], key))
	}

	// Replace the certificates
	pub fn set_certs(&self, certs: Vec<Arc<CertifiedKey>>) {
		let fingerprints = certs
			.iter()
			.map(|ck| {
				let fingerprint = crate::crypto::sha256(&self.provider, ck.cert[0].as_ref());
				hex::encode(fingerprint)
			})
			.collect();

		let mut info = self.info.write().expect("info write lock poisoned");
		info.certs = certs;
		info.fingerprints = fingerprints;
	}

	// Return the best certificate for the given ClientHello.
	fn best_certificate(&self, client_hello: &ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
		let server_name = client_hello.server_name()?;
		let dns_name = rustls::pki_types::ServerName::try_from(server_name).ok()?;

		for ck in self.info.read().expect("info read lock poisoned").certs.iter() {
			let leaf: webpki::EndEntityCert = ck
				.end_entity_cert()
				.expect("missing certificate")
				.try_into()
				.expect("failed to parse certificate");

			if leaf.verify_is_valid_for_subject_name(&dns_name).is_ok() {
				return Some(ck.clone());
			}
		}

		None
	}
}

impl ResolvesServerCert for ServeCerts {
	fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
		if let Some(cert) = self.best_certificate(&client_hello) {
			return Some(cert);
		}

		// If this happens, it means the client was trying to connect to an unknown hostname.
		// We do our best and return the first certificate.
		tracing::warn!(server_name = ?client_hello.server_name(), "no SNI certificate found");

		self.info
			.read()
			.expect("info read lock poisoned")
			.certs
			.first()
			.cloned()
	}
}
