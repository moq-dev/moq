use std::path::PathBuf;
use std::{net, time::Duration};

use crate::QuicBackend;
use crate::crypto;
#[cfg(feature = "iroh")]
use crate::iroh::IrohQuicRequest;
use anyhow::Context;
use moq_lite::Session;
use rustls::pki_types::CertificateDer;
#[cfg(feature = "quinn")]
use rustls::pki_types::PrivatePkcs8KeyDer;
use std::fs;
use std::io::{self, Cursor, Read};
use std::sync::{Arc, RwLock};
use url::Url;
#[cfg(feature = "iroh")]
use web_transport_iroh::iroh;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};

/// TLS configuration for the server.
///
/// Certificate and keys must currently be files on disk.
/// Alternatively, you can generate a self-signed certificate given a list of hostnames.
#[derive(clap::Args, Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
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

/// Configuration for the MoQ server.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ServerConfig {
	/// Listen for UDP packets on the given address.
	/// Defaults to `[::]:443` if not provided.
	#[serde(alias = "listen")]
	#[arg(id = "server-bind", long = "server-bind", alias = "listen", env = "MOQ_SERVER_BIND")]
	pub bind: Option<net::SocketAddr>,

	/// The QUIC backend to use.
	#[arg(long = "quic-backend", default_value = "quinn", env = "MOQ_QUIC_BACKEND")]
	pub backend: QuicBackend,

	/// Server ID to embed in connection IDs for QUIC-LB compatibility.
	/// If set, connection IDs will be derived semi-deterministically.
	#[arg(id = "server-quic-lb-id", long = "server-quic-lb-id", env = "MOQ_SERVER_QUIC_LB_ID")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic_lb_id: Option<ServerId>,

	/// Number of random nonce bytes in QUIC-LB connection IDs.
	/// Must be at least 4, and server_id + nonce + 1 must not exceed 20.
	#[arg(
		id = "server-quic-lb-nonce",
		long = "server-quic-lb-nonce",
		requires = "server-quic-lb-id",
		env = "MOQ_SERVER_QUIC_LB_NONCE"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic_lb_nonce: Option<usize>,

	#[command(flatten)]
	#[serde(default)]
	pub tls: ServerTlsConfig,
}

impl ServerConfig {
	pub fn init(self) -> anyhow::Result<Server> {
		match self.backend {
			QuicBackend::Quinn => {
				#[cfg(not(feature = "quinn"))]
				anyhow::bail!("quinn backend not compiled; rebuild with --features quinn");

				#[cfg(feature = "quinn")]
				Server::new_quinn(self)
			}
			QuicBackend::Quiche => {
				#[cfg(not(feature = "quiche"))]
				anyhow::bail!("quiche backend not compiled; rebuild with --features quiche");

				#[cfg(feature = "quiche")]
				Server::new_quiche(self)
			}
		}
	}
}

/// Server for accepting MoQ connections over QUIC.
///
/// Create via [`ServerConfig::init`] or [`Server::new`].
pub struct Server {
	moq: moq_lite::Server,
	inner: ServerInner,
	accept: FuturesUnordered<BoxFuture<'static, anyhow::Result<Request>>>,
	#[cfg(feature = "iroh")]
	iroh: Option<iroh::Endpoint>,
}

enum ServerInner {
	#[cfg(feature = "quinn")]
	Quinn {
		quic: quinn::Endpoint,
		certs: Arc<ServeCerts>,
	},
	#[cfg(feature = "quiche")]
	Quiche {
		server: web_transport_quiche::ez::Server,
		fingerprints: Arc<RwLock<ServerTlsInfo>>,
	},
}

impl Server {
	/// Create a new server using the default (quinn) backend.
	#[cfg(feature = "quinn")]
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		Self::new_quinn(config)
	}

	#[cfg(feature = "quinn")]
	fn new_quinn(config: ServerConfig) -> anyhow::Result<Self> {
		// Enable BBR congestion control
		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
		transport.keep_alive_interval(Some(Duration::from_secs(4)));
		transport.mtu_discovery_config(None); // Disable MTU discovery
		let transport = Arc::new(transport);

		let provider = crypto::provider();

		let certs = ServeCerts::new(provider.clone());

		certs.load_certs(&config.tls)?;

		let certs = Arc::new(certs);

		#[cfg(unix)]
		tokio::spawn(Self::reload_certs_quinn(certs.clone(), config.tls.clone()));

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

		// Configure connection ID generator with server ID if provided
		let mut endpoint_config = quinn::EndpointConfig::default();
		if let Some(server_id) = config.quic_lb_id {
			let nonce_len = config.quic_lb_nonce.unwrap_or(8);
			anyhow::ensure!(nonce_len >= 4, "quic_lb_nonce must be at least 4");

			let cid_len = 1 + server_id.len() + nonce_len;
			anyhow::ensure!(cid_len <= 20, "connection ID length ({cid_len}) exceeds maximum of 20");

			tracing::info!(
				?server_id,
				nonce_len,
				"using QUIC-LB compatible connection ID generation"
			);
			endpoint_config.cid_generator(move || Box::new(ServerIdGenerator::new(server_id.clone(), nonce_len)));
		}

		let listen = config.bind.unwrap_or("[::]:443".parse().unwrap());
		let socket = std::net::UdpSocket::bind(listen).context("failed to bind UDP socket")?;

		// Create the generic QUIC endpoint.
		let quic = quinn::Endpoint::new(endpoint_config, Some(tls), socket, runtime)
			.context("failed to create QUIC endpoint")?;

		Ok(Self {
			inner: ServerInner::Quinn { quic, certs },
			accept: Default::default(),
			moq: moq_lite::Server::new(),
			#[cfg(feature = "iroh")]
			iroh: None,
		})
	}

	#[cfg(feature = "quiche")]
	fn new_quiche(config: ServerConfig) -> anyhow::Result<Self> {
		if config.quic_lb_id.is_some() {
			tracing::warn!("QUIC-LB is not supported with the quiche backend; ignoring server ID");
		}

		if !config.tls.generate.is_empty() {
			anyhow::bail!(
				"--tls-generate is not supported with the quiche backend (requires rcgen which is quinn-gated)"
			);
		}

		anyhow::ensure!(
			!config.tls.cert.is_empty() && !config.tls.key.is_empty(),
			"--tls-cert and --tls-key are required with the quiche backend"
		);
		anyhow::ensure!(
			config.tls.cert.len() == config.tls.key.len(),
			"must provide matching --tls-cert and --tls-key pairs"
		);

		let listen = config.bind.unwrap_or("[::]:443".parse().unwrap());

		// Load certs in PEM format and convert to DER for quiche
		let (chain, key) = Self::load_quiche_cert(&config.tls.cert[0], &config.tls.key[0])?;

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
			inner: ServerInner::Quiche {
				server,
				fingerprints: info,
			},
			accept: Default::default(),
			moq: moq_lite::Server::new(),
			#[cfg(feature = "iroh")]
			iroh: None,
		})
	}

	#[cfg(feature = "quiche")]
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

	#[cfg(all(unix, feature = "quinn"))]
	async fn reload_certs_quinn(certs: Arc<ServeCerts>, tls_config: ServerTlsConfig) {
		use tokio::signal::unix::{SignalKind, signal};

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
	pub fn tls_info(&self) -> Arc<RwLock<ServerTlsInfo>> {
		match &self.inner {
			#[cfg(feature = "quinn")]
			ServerInner::Quinn { certs, .. } => certs.info.clone(),
			#[cfg(feature = "quiche")]
			ServerInner::Quiche { fingerprints, .. } => fingerprints.clone(),
		}
	}

	/// Returns the next partially established QUIC or WebTransport session.
	///
	/// This returns a [Request] instead of a [web_transport_quinn::Session]
	/// so the connection can be rejected early on an invalid path or missing auth.
	///
	/// The [Request] is either a WebTransport or a raw QUIC request.
	/// Call [Request::accept] or [Request::reject] to complete the handshake.
	pub async fn accept(&mut self) -> Option<Request> {
		loop {
			// tokio::select! does not support cfg directives on arms, so we need to put the
			// iroh cfg into a block, and default to a pending future if iroh is disabled.
			let iroh_accept_fut = async {
				#[cfg(feature = "iroh")]
				if let Some(endpoint) = self.iroh.as_ref() {
					endpoint.accept().await
				} else {
					std::future::pending::<_>().await
				}

				#[cfg(not(feature = "iroh"))]
				std::future::pending::<()>().await
			};

			match &mut self.inner {
				#[cfg(feature = "quinn")]
				ServerInner::Quinn { quic, .. } => {
					tokio::select! {
						res = quic.accept() => {
							let conn = res?;
							self.accept.push(Self::accept_quinn_session(self.moq.clone(), conn).boxed());
						}
						res = iroh_accept_fut => {
							#[cfg(feature = "iroh")]
							{
								let conn = res?;
								self.accept.push(Self::accept_iroh_session(self.moq.clone(), conn).boxed());
							}
							#[cfg(not(feature = "iroh"))]
							let _: () = res;
						}
						Some(res) = self.accept.next() => {
							match res {
								Ok(session) => return Some(session),
								Err(err) => tracing::debug!(%err, "failed to accept session"),
							}
						}
						_ = tokio::signal::ctrl_c() => {
							self.close();
							tokio::time::sleep(Duration::from_millis(100)).await;
							return None;
						}
					}
				}
				#[cfg(feature = "quiche")]
				ServerInner::Quiche { server, .. } => {
					tokio::select! {
						res = server.accept() => {
							let conn = res?;
							self.accept.push(Self::accept_quiche_session(self.moq.clone(), conn).boxed());
						}
						res = iroh_accept_fut => {
							#[cfg(feature = "iroh")]
							{
								let conn = res?;
								self.accept.push(Self::accept_iroh_session(self.moq.clone(), conn).boxed());
							}
							#[cfg(not(feature = "iroh"))]
							let _: () = res;
						}
						Some(res) = self.accept.next() => {
							match res {
								Ok(session) => return Some(session),
								Err(err) => tracing::debug!(%err, "failed to accept session"),
							}
						}
						_ = tokio::signal::ctrl_c() => {
							self.close();
							tokio::time::sleep(Duration::from_millis(100)).await;
							return None;
						}
					}
				}
			}
		}
	}

	#[cfg(feature = "quinn")]
	async fn accept_quinn_session(server: moq_lite::Server, conn: quinn::Incoming) -> anyhow::Result<Request> {
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
				Ok(Request {
					server: server.clone(),
					kind: RequestKind::QuinnWebTransport(request),
				})
			}
			moq_lite::lite::ALPN | moq_lite::ietf::ALPN => Ok(Request {
				server: server.clone(),
				kind: RequestKind::QuinnQuic(QuicRequest::accept(conn)),
			}),
			_ => anyhow::bail!("unsupported ALPN: {alpn}"),
		}
	}

	#[cfg(feature = "quiche")]
	async fn accept_quiche_session(
		server: moq_lite::Server,
		conn: web_transport_quiche::ez::Connection,
	) -> anyhow::Result<Request> {
		let alpn = conn.alpn().unwrap_or_default();
		let alpn = String::from_utf8(alpn).unwrap_or_default();

		tracing::debug!(ip = %conn.peer_addr(), %alpn, "accepting via quiche");

		match alpn.as_str() {
			"h3" => {
				// WebTransport over HTTP/3: perform the H3 handshake
				let request = web_transport_quiche::h3::Request::accept(conn)
					.await
					.map_err(|e| anyhow::anyhow!("failed to accept WebTransport request: {e}"))?;
				Ok(Request {
					server: server.clone(),
					kind: RequestKind::QuicheWebTransport(request),
				})
			}
			_ => {
				// Raw QUIC mode
				Ok(Request {
					server: server.clone(),
					kind: RequestKind::QuicheQuic(crate::quiche::QuicheQuicRequest::accept(conn)),
				})
			}
		}
	}

	#[cfg(feature = "iroh")]
	async fn accept_iroh_session(server: moq_lite::Server, conn: iroh::endpoint::Incoming) -> anyhow::Result<Request> {
		let conn = conn.accept()?.await?;
		let alpn = String::from_utf8(conn.alpn().to_vec()).context("failed to decode ALPN")?;
		tracing::Span::current().record("id", conn.stable_id());
		tracing::debug!(remote = %conn.remote_id().fmt_short(), %alpn, "accepted");
		match alpn.as_str() {
			web_transport_iroh::ALPN_H3 => {
				let request = web_transport_iroh::H3Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Request {
					server: server.clone(),
					kind: RequestKind::IrohWebTransport(request),
				})
			}
			moq_lite::lite::ALPN | moq_lite::ietf::ALPN => {
				let request = IrohQuicRequest::accept(conn);
				Ok(Request {
					server: server.clone(),
					kind: RequestKind::IrohQuic(request),
				})
			}
			_ => Err(anyhow::anyhow!("unsupported ALPN: {alpn}")),
		}
	}

	#[cfg(feature = "iroh")]
	pub fn iroh_endpoint(&self) -> Option<&iroh::Endpoint> {
		self.iroh.as_ref()
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		match &self.inner {
			#[cfg(feature = "quinn")]
			ServerInner::Quinn { quic, .. } => quic.local_addr().context("failed to get local address"),
			#[cfg(feature = "quiche")]
			ServerInner::Quiche { server, .. } => server.local_addr().context("failed to get local address"),
		}
	}

	pub fn close(&mut self) {
		match &mut self.inner {
			#[cfg(feature = "quinn")]
			ServerInner::Quinn { quic, .. } => {
				quic.close(quinn::VarInt::from_u32(0), b"server shutdown");
			}
			#[cfg(feature = "quiche")]
			ServerInner::Quiche { .. } => {
				// quiche server doesn't have a close method; dropping it is sufficient
			}
		}
	}
}

/// An incoming connection that can be accepted or rejected.
enum RequestKind {
	#[cfg(feature = "quinn")]
	QuinnWebTransport(web_transport_quinn::Request),
	#[cfg(feature = "quinn")]
	QuinnQuic(QuicRequest),
	#[cfg(feature = "quiche")]
	QuicheWebTransport(web_transport_quiche::h3::Request),
	#[cfg(feature = "quiche")]
	QuicheQuic(crate::quiche::QuicheQuicRequest),
	#[cfg(feature = "iroh")]
	IrohWebTransport(web_transport_iroh::H3Request),
	#[cfg(feature = "iroh")]
	IrohQuic(IrohQuicRequest),
}

pub struct Request {
	server: moq_lite::Server,
	kind: RequestKind,
}

impl Request {
	/// Reject the session, returning your favorite HTTP status code.
	pub async fn reject(self, code: u16) -> anyhow::Result<()> {
		match self.kind {
			#[cfg(feature = "quinn")]
			RequestKind::QuinnWebTransport(request) => {
				let status = web_transport_quinn::http::StatusCode::from_u16(code).context("invalid status code")?;
				request.close(status).await?;
			}
			#[cfg(feature = "quinn")]
			RequestKind::QuinnQuic(request) => {
				let status = web_transport_quinn::http::StatusCode::from_u16(code).context("invalid status code")?;
				request.close(status);
			}
			#[cfg(feature = "quiche")]
			RequestKind::QuicheWebTransport(request) => {
				let status = web_transport_quiche::http::StatusCode::from_u16(code).context("invalid status code")?;
				request
					.close(status)
					.await
					.map_err(|e| anyhow::anyhow!("failed to close quiche WebTransport request: {e}"))?;
			}
			#[cfg(feature = "quiche")]
			RequestKind::QuicheQuic(request) => {
				let status = web_transport_quiche::http::StatusCode::from_u16(code).context("invalid status code")?;
				request.close(status);
			}
			#[cfg(feature = "iroh")]
			RequestKind::IrohWebTransport(request) => {
				let status = web_transport_iroh::http::StatusCode::from_u16(code).context("invalid status code")?;
				request.close(status).await?;
			}
			#[cfg(feature = "iroh")]
			RequestKind::IrohQuic(request) => {
				let status = web_transport_iroh::http::StatusCode::from_u16(code).context("invalid status code")?;
				request.close(status);
			}
		}
		Ok(())
	}

	pub fn with_publish(mut self, publish: impl Into<Option<moq_lite::OriginConsumer>>) -> Self {
		self.server = self.server.with_publish(publish);
		self
	}

	pub fn with_consume(mut self, consume: impl Into<Option<moq_lite::OriginProducer>>) -> Self {
		self.server = self.server.with_consume(consume);
		self
	}

	/// Accept the session, performing rest of the MoQ handshake.
	pub async fn accept(self) -> anyhow::Result<Session> {
		let session = match self.kind {
			#[cfg(feature = "quinn")]
			RequestKind::QuinnWebTransport(request) => self.server.accept(request.ok().await?).await?,
			#[cfg(feature = "quinn")]
			RequestKind::QuinnQuic(request) => self.server.accept(request.ok()).await?,
			#[cfg(feature = "quiche")]
			RequestKind::QuicheWebTransport(request) => {
				let conn = request
					.respond(web_transport_quiche::http::StatusCode::OK)
					.await
					.map_err(|e| anyhow::anyhow!("failed to accept quiche WebTransport: {e}"))?;
				self.server.accept(conn).await?
			}
			#[cfg(feature = "quiche")]
			RequestKind::QuicheQuic(request) => self.server.accept(request.ok()).await?,
			#[cfg(feature = "iroh")]
			RequestKind::IrohWebTransport(request) => self.server.accept(request.ok().await?).await?,
			#[cfg(feature = "iroh")]
			RequestKind::IrohQuic(request) => self.server.accept(request.ok()).await?,
		};
		Ok(session)
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> Option<&Url> {
		match &self.kind {
			#[cfg(feature = "quinn")]
			RequestKind::QuinnWebTransport(request) => Some(request.url()),
			#[cfg(feature = "quiche")]
			RequestKind::QuicheWebTransport(request) => Some(request.url()),
			#[cfg(feature = "iroh")]
			RequestKind::IrohWebTransport(request) => Some(request.url()),
			_ => None,
		}
	}
}

/// A raw QUIC connection request without WebTransport framing (quinn backend).
#[cfg(feature = "quinn")]
pub struct QuicRequest {
	connection: quinn::Connection,
	url: Url,
}

#[cfg(feature = "quinn")]
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
	pub fn close(self, status: web_transport_quinn::http::StatusCode) {
		self.connection
			.close(status.as_u16().into(), status.as_str().as_bytes());
	}
}

/// TLS certificate information including fingerprints.
#[derive(Debug)]
pub struct ServerTlsInfo {
	#[cfg(feature = "quinn")]
	pub(crate) certs: Vec<Arc<rustls::sign::CertifiedKey>>,
	pub fingerprints: Vec<String>,
}

#[cfg(feature = "quinn")]
#[derive(Debug)]
struct ServeCerts {
	info: Arc<RwLock<ServerTlsInfo>>,
	provider: crypto::Provider,
}

#[cfg(feature = "quinn")]
impl ServeCerts {
	pub fn new(provider: crypto::Provider) -> Self {
		Self {
			info: Arc::new(RwLock::new(ServerTlsInfo {
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
	fn load(&self, chain_path: &PathBuf, key_path: &PathBuf) -> anyhow::Result<rustls::sign::CertifiedKey> {
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

		let certified_key = rustls::sign::CertifiedKey::new(chain, key);

		certified_key.keys_match().context(format!(
			"private key {} doesn't match certificate {}",
			key_path.display(),
			chain_path.display()
		))?;

		Ok(certified_key)
	}

	fn generate(&self, hostnames: &[String]) -> anyhow::Result<rustls::sign::CertifiedKey> {
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
		Ok(rustls::sign::CertifiedKey::new(vec![cert.into()], key))
	}

	// Replace the certificates
	pub fn set_certs(&self, certs: Vec<Arc<rustls::sign::CertifiedKey>>) {
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
	fn best_certificate(
		&self,
		client_hello: &rustls::server::ClientHello<'_>,
	) -> Option<Arc<rustls::sign::CertifiedKey>> {
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

#[cfg(feature = "quinn")]
impl rustls::server::ResolvesServerCert for ServeCerts {
	fn resolve(&self, client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
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

/// Server ID for QUIC-LB support.
#[serde_with::serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerId(#[serde_as(as = "serde_with::hex::Hex")] Vec<u8>);

impl ServerId {
	#[cfg(feature = "quinn")]
	fn len(&self) -> usize {
		self.0.len()
	}
}

impl std::fmt::Debug for ServerId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("QuicLbServerId").field(&hex::encode(&self.0)).finish()
	}
}

impl std::str::FromStr for ServerId {
	type Err = hex::FromHexError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		hex::decode(s).map(Self)
	}
}

/// Connection ID generator that embeds a fixed server ID for QUIC-LB support.
#[cfg(feature = "quinn")]
struct ServerIdGenerator {
	server_id: ServerId,
	nonce_len: usize,
}

#[cfg(feature = "quinn")]
impl ServerIdGenerator {
	fn new(server_id: ServerId, nonce_len: usize) -> Self {
		Self { server_id, nonce_len }
	}
}

#[cfg(feature = "quinn")]
impl quinn::ConnectionIdGenerator for ServerIdGenerator {
	fn generate_cid(&mut self) -> quinn::ConnectionId {
		use rand::Rng;
		let cid_len = self.cid_len();
		let mut cid = Vec::with_capacity(cid_len);
		// First byte has "self-encoded length" of server ID + nonce
		cid.push((cid_len - 1) as u8);
		cid.extend(self.server_id.0.iter());
		cid.extend(rand::rng().random_iter::<u8>().take(self.nonce_len));
		quinn::ConnectionId::new(cid.as_slice())
	}

	fn cid_len(&self) -> usize {
		1 + self.server_id.len() + self.nonce_len
	}

	fn cid_lifetime(&self) -> Option<Duration> {
		None
	}
}
