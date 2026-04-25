use std::net;
use std::path::PathBuf;

use crate::QuicBackend;
use moq_lite::Session;
use std::sync::{Arc, RwLock};
use url::Url;
#[cfg(feature = "iroh")]
use web_transport_iroh::iroh;

use anyhow::Context;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;

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

	/// PEM file(s) of root CAs for validating optional client certificates (mTLS).
	///
	/// When set, clients *may* present a certificate during the TLS handshake.
	/// Valid presentations are exposed via [`Request::peer_identity`] and can be
	/// used by the application to grant elevated access. Clients that do not
	/// present a certificate are unaffected.
	///
	/// Only supported by the Quinn backend.
	#[arg(
		long = "server-tls-root",
		id = "server-tls-root",
		value_delimiter = ',',
		env = "MOQ_SERVER_TLS_ROOT"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub root: Vec<PathBuf>,
}

impl ServerTlsConfig {
	/// Load all configured root CAs into a [`rustls::RootCertStore`].
	pub fn load_roots(&self) -> anyhow::Result<rustls::RootCertStore> {
		use rustls::pki_types::CertificateDer;

		let mut roots = rustls::RootCertStore::empty();
		for path in &self.root {
			let file = std::fs::File::open(path).context("failed to open root CA")?;
			let mut reader = std::io::BufReader::new(file);
			let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
				.collect::<Result<_, _>>()
				.context("failed to parse root CA PEM")?;
			anyhow::ensure!(!certs.is_empty(), "no certificates found in root CA");
			for cert in certs {
				roots.add(cert).context("failed to add root CA")?;
			}
		}
		Ok(roots)
	}
}

/// Configuration for the MoQ server.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ServerConfig {
	/// Listen for UDP packets on the given address.
	/// Defaults to `[::]:443` if not provided.
	///
	/// Accepts standard socket address syntax (e.g. `[::]:443`) or a DNS
	/// `host:port` pair (e.g. `fly-global-services:443`), which is resolved
	/// at bind time. Only the first resolved address is used; Quinn does not
	/// support binding to multiple addresses.
	#[serde(alias = "listen")]
	#[arg(id = "server-bind", long = "server-bind", alias = "listen", env = "MOQ_SERVER_BIND")]
	pub bind: Option<String>,

	/// The QUIC backend to use.
	/// Auto-detected from compiled features if not specified.
	#[arg(id = "server-backend", long = "server-backend", env = "MOQ_SERVER_BACKEND")]
	pub backend: Option<QuicBackend>,

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

	/// Maximum number of concurrent QUIC streams per connection (both bidi and uni).
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "server-max-streams",
		long = "server-max-streams",
		env = "MOQ_SERVER_MAX_STREAMS"
	)]
	pub max_streams: Option<u64>,

	/// Restrict the server to specific MoQ protocol version(s).
	///
	/// By default, the server accepts all supported versions.
	/// Use this to restrict to specific versions, e.g. `--server-version moq-lite-02`.
	/// Can be specified multiple times to accept a subset of versions.
	///
	/// Valid values: moq-lite-01, moq-lite-02, moq-lite-03, moq-transport-14, moq-transport-15, moq-transport-16
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "server-version", long = "server-version", env = "MOQ_SERVER_VERSION")]
	pub version: Vec<moq_lite::Version>,

	#[command(flatten)]
	#[serde(default)]
	pub tls: ServerTlsConfig,
}

impl ServerConfig {
	pub fn init(self) -> anyhow::Result<Server> {
		Server::new(self)
	}

	/// Returns the configured versions, defaulting to all if none specified.
	pub fn versions(&self) -> moq_lite::Versions {
		if self.version.is_empty() {
			moq_lite::Versions::all()
		} else {
			moq_lite::Versions::from(self.version.clone())
		}
	}
}

/// Default bind address used when [`ServerConfig::bind`] is not set.
pub(crate) const DEFAULT_BIND: &str = "[::]:443";

/// Server for accepting MoQ connections over QUIC.
///
/// Create via [`ServerConfig::init`] or [`Server::new`].
pub struct Server {
	moq: moq_lite::Server,
	versions: moq_lite::Versions,
	accept: FuturesUnordered<BoxFuture<'static, anyhow::Result<Request>>>,
	#[cfg(feature = "iroh")]
	iroh: Option<iroh::Endpoint>,
	#[cfg(feature = "noq")]
	noq: Option<crate::noq::NoqServer>,
	#[cfg(feature = "quinn")]
	quinn: Option<crate::quinn::QuinnServer>,
	#[cfg(feature = "quiche")]
	quiche: Option<crate::quiche::QuicheServer>,
	#[cfg(feature = "websocket")]
	websocket: Option<crate::websocket::WebSocketListener>,
}

impl Server {
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		let backend = config.backend.clone().unwrap_or({
			#[cfg(feature = "quinn")]
			{
				QuicBackend::Quinn
			}
			#[cfg(all(feature = "noq", not(feature = "quinn")))]
			{
				QuicBackend::Noq
			}
			#[cfg(all(feature = "quiche", not(feature = "quinn"), not(feature = "noq")))]
			{
				QuicBackend::Quiche
			}
			#[cfg(all(not(feature = "quiche"), not(feature = "quinn"), not(feature = "noq")))]
			panic!("no QUIC backend compiled; enable noq, quinn, or quiche feature");
		});

		let versions = config.versions();

		if !config.tls.root.is_empty() {
			#[cfg(feature = "quinn")]
			let quinn_backend = matches!(backend, QuicBackend::Quinn);
			#[cfg(not(feature = "quinn"))]
			let quinn_backend = false;
			anyhow::ensure!(quinn_backend, "tls.root (mTLS) is only supported by the quinn backend");
		}

		#[cfg(feature = "noq")]
		#[allow(unreachable_patterns)]
		let noq = match backend {
			QuicBackend::Noq => Some(crate::noq::NoqServer::new(config.clone())?),
			_ => None,
		};

		#[cfg(feature = "quinn")]
		#[allow(unreachable_patterns)]
		let quinn = match backend {
			QuicBackend::Quinn => Some(crate::quinn::QuinnServer::new(config.clone())?),
			_ => None,
		};

		#[cfg(feature = "quiche")]
		let quiche = match backend {
			QuicBackend::Quiche => Some(crate::quiche::QuicheServer::new(config)?),
			_ => None,
		};

		Ok(Server {
			accept: Default::default(),
			moq: moq_lite::Server::new().with_versions(versions.clone()),
			versions,
			#[cfg(feature = "iroh")]
			iroh: None,
			#[cfg(feature = "noq")]
			noq,
			#[cfg(feature = "quinn")]
			quinn,
			#[cfg(feature = "quiche")]
			quiche,
			#[cfg(feature = "websocket")]
			websocket: None,
		})
	}

	/// Add a standalone WebSocket listener on a separate TCP port.
	///
	/// This is useful for simple applications that want WebSocket on a dedicated port.
	/// For applications that need WebSocket on the same HTTP port (e.g. moq-relay),
	/// use `qmux::Session::accept()` with your own HTTP framework instead.
	#[cfg(feature = "websocket")]
	pub fn with_websocket(mut self, websocket: Option<crate::websocket::WebSocketListener>) -> Self {
		self.websocket = websocket;
		self
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

	// Return the SHA256 fingerprints of all our certificates.
	pub fn tls_info(&self) -> Arc<RwLock<ServerTlsInfo>> {
		#[cfg(feature = "noq")]
		if let Some(noq) = self.noq.as_ref() {
			return noq.tls_info();
		}
		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_ref() {
			return quinn.tls_info();
		}
		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_ref() {
			return quiche.tls_info();
		}
		unreachable!("no QUIC backend compiled");
	}

	#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche", feature = "iroh")))]
	pub async fn accept(&mut self) -> Option<Request> {
		unreachable!("no QUIC backend compiled; enable noq, quinn, quiche, or iroh feature");
	}

	/// Returns the next partially established QUIC or WebTransport session.
	///
	/// This returns a [Request] instead of a [web_transport_quinn::Session]
	/// so the connection can be rejected early on an invalid path or missing auth.
	///
	/// The [Request] is either a WebTransport or a raw QUIC request.
	/// Call [Request::ok] or [Request::close] to complete the handshake.
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche", feature = "iroh"))]
	pub async fn accept(&mut self) -> Option<Request> {
		loop {
			// tokio::select! does not support cfg directives on arms, so we need to create the futures here.
			#[cfg(feature = "noq")]
			let noq_accept = async {
				#[cfg(feature = "noq")]
				if let Some(noq) = self.noq.as_mut() {
					return noq.accept().await;
				}
				None
			};
			#[cfg(not(feature = "noq"))]
			let noq_accept = async { None::<()> };

			#[cfg(feature = "iroh")]
			let iroh_accept = async {
				#[cfg(feature = "iroh")]
				if let Some(endpoint) = self.iroh.as_mut() {
					return endpoint.accept().await;
				}
				None
			};
			#[cfg(not(feature = "iroh"))]
			let iroh_accept = async { None::<()> };

			#[cfg(feature = "quinn")]
			let quinn_accept = async {
				#[cfg(feature = "quinn")]
				if let Some(quinn) = self.quinn.as_mut() {
					return quinn.accept().await;
				}
				None
			};
			#[cfg(not(feature = "quinn"))]
			let quinn_accept = async { None::<()> };

			#[cfg(feature = "quiche")]
			let quiche_accept = async {
				#[cfg(feature = "quiche")]
				if let Some(quiche) = self.quiche.as_mut() {
					return quiche.accept().await;
				}
				None
			};
			#[cfg(not(feature = "quiche"))]
			let quiche_accept = async { None::<()> };

			#[cfg(feature = "websocket")]
			let ws_ref = self.websocket.as_ref();
			#[cfg(feature = "websocket")]
			let ws_accept = async {
				match ws_ref {
					Some(ws) => ws.accept().await,
					None => std::future::pending().await,
				}
			};
			#[cfg(not(feature = "websocket"))]
			let ws_accept = std::future::pending::<Option<anyhow::Result<()>>>();

			let server = self.moq.clone();
			let versions = self.versions.clone();

			tokio::select! {
				Some(_conn) = noq_accept => {
					#[cfg(feature = "noq")]
					{
						let alpns = versions.alpns();
						self.accept.push(async move {
							let noq = super::noq::NoqRequest::accept(_conn, alpns).await?;
							Ok(Request {
								server,
								kind: RequestKind::Noq(noq),
							})
						}.boxed());
					}
				}
				Some(_conn) = quinn_accept => {
					#[cfg(feature = "quinn")]
					{
						let alpns = versions.alpns();
						self.accept.push(async move {
							let quinn = super::quinn::QuinnRequest::accept(_conn, alpns).await?;
							Ok(Request {
								server,
								kind: RequestKind::Quinn(Box::new(quinn)),
							})
						}.boxed());
					}
				}
				Some(_conn) = quiche_accept => {
					#[cfg(feature = "quiche")]
					{
						let alpns = versions.alpns();
						self.accept.push(async move {
							let quiche = super::quiche::QuicheRequest::accept(_conn, alpns).await?;
							Ok(Request {
								server,
								kind: RequestKind::Quiche(quiche),
							})
						}.boxed());
					}
				}
				Some(_conn) = iroh_accept => {
					#[cfg(feature = "iroh")]
					self.accept.push(async move {
						let iroh = super::iroh::IrohRequest::accept(_conn).await?;
						Ok(Request {
							server,
							kind: RequestKind::Iroh(iroh),
						})
					}.boxed());
				}
				Some(_res) = ws_accept => {
					#[cfg(feature = "websocket")]
					match _res {
						Ok(session) => {
							return Some(Request {
								server,
								kind: RequestKind::WebSocket(session),
							});
						}
						Err(err) => tracing::debug!(%err, "failed to accept WebSocket session"),
					}
				}
				Some(res) = self.accept.next() => {
					match res {
						Ok(session) => return Some(session),
						Err(err) => tracing::debug!(%err, "failed to accept session"),
					}
				}
				_ = tokio::signal::ctrl_c() => {
					self.close().await;
					return None;
				}
			}
		}
	}

	#[cfg(feature = "iroh")]
	pub fn iroh_endpoint(&self) -> Option<&iroh::Endpoint> {
		self.iroh.as_ref()
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		#[cfg(feature = "noq")]
		if let Some(noq) = self.noq.as_ref() {
			return noq.local_addr();
		}
		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_ref() {
			return quinn.local_addr();
		}
		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_ref() {
			return quiche.local_addr();
		}
		unreachable!("no QUIC backend compiled");
	}

	#[cfg(feature = "websocket")]
	pub fn websocket_local_addr(&self) -> Option<net::SocketAddr> {
		self.websocket.as_ref().and_then(|ws| ws.local_addr().ok())
	}

	pub async fn close(&mut self) {
		#[cfg(feature = "noq")]
		if let Some(noq) = self.noq.as_mut() {
			noq.close();
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		}
		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_mut() {
			quinn.close();
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		}
		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_mut() {
			quiche.close();
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		}
		#[cfg(feature = "iroh")]
		if let Some(iroh) = self.iroh.take() {
			iroh.close().await;
		}
		#[cfg(feature = "websocket")]
		{
			let _ = self.websocket.take();
		}
		#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche", feature = "iroh")))]
		unreachable!("no QUIC backend compiled");
	}
}

/// The identity of a peer that presented a client certificate during the TLS
/// handshake, as validated against the configured [`ServerTlsConfig::root`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct PeerIdentity {}

/// An incoming connection that can be accepted or rejected.
pub(crate) enum RequestKind {
	#[cfg(feature = "noq")]
	Noq(crate::noq::NoqRequest),
	#[cfg(feature = "quinn")]
	Quinn(Box<crate::quinn::QuinnRequest>),
	#[cfg(feature = "quiche")]
	Quiche(crate::quiche::QuicheRequest),
	#[cfg(feature = "iroh")]
	Iroh(crate::iroh::IrohRequest),
	#[cfg(feature = "websocket")]
	WebSocket(qmux::Session),
}

/// An incoming MoQ session that can be accepted or rejected.
///
/// [Self::with_publish] and [Self::with_consume] will configure what will be published and consumed from the session respectively.
/// Otherwise, the Server's configuration is used by default.
pub struct Request {
	server: moq_lite::Server,
	kind: RequestKind,
}

impl Request {
	/// Reject the session, returning your favorite HTTP status code.
	pub async fn close(self, _code: u16) -> anyhow::Result<()> {
		match self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq(request) => {
				let status = web_transport_noq::http::StatusCode::from_u16(_code).context("invalid status code")?;
				request.close(status).await?;
				Ok(())
			}
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(request) => {
				let status = web_transport_quinn::http::StatusCode::from_u16(_code).context("invalid status code")?;
				request.close(status).await?;
				Ok(())
			}
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(request) => {
				let status = web_transport_quiche::http::StatusCode::from_u16(_code).context("invalid status code")?;
				request
					.reject(status)
					.await
					.map_err(|e| anyhow::anyhow!("failed to close quiche WebTransport request: {e}"))?;
				Ok(())
			}
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(request) => {
				let status = web_transport_iroh::http::StatusCode::from_u16(_code).context("invalid status code")?;
				request.close(status).await?;
				Ok(())
			}
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(_session) => {
				// WebSocket doesn't support HTTP status codes; just drop to close.
				Ok(())
			}
		}
	}

	/// Publish the given origin to the session.
	pub fn with_publish(mut self, publish: impl Into<Option<moq_lite::OriginConsumer>>) -> Self {
		self.server = self.server.with_publish(publish);
		self
	}

	/// Consume the given origin from the session.
	pub fn with_consume(mut self, consume: impl Into<Option<moq_lite::OriginProducer>>) -> Self {
		self.server = self.server.with_consume(consume);
		self
	}

	/// Accept the session, performing rest of the MoQ handshake.
	pub async fn ok(self) -> anyhow::Result<Session> {
		match self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq(request) => Ok(self.server.accept(request.ok().await?).await?),
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(request) => Ok(self.server.accept(request.ok().await?).await?),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(request) => {
				let conn = request
					.ok()
					.await
					.map_err(|e| anyhow::anyhow!("failed to accept quiche WebTransport: {e}"))?;
				Ok(self.server.accept(conn).await?)
			}
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(request) => Ok(self.server.accept(request.ok().await?).await?),
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(session) => Ok(self.server.accept(session).await?),
		}
	}

	/// Returns the transport type as a string (e.g. "quic", "iroh").
	pub fn transport(&self) -> &'static str {
		match self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq(_) => "quic",
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(_) => "quic",
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(_) => "quic",
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(_) => "iroh",
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(_) => "websocket",
		}
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> Option<&Url> {
		#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche", feature = "iroh")))]
		unreachable!("no QUIC backend compiled; enable noq, quinn, quiche, or iroh feature");

		match self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq(ref request) => request.url(),
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(ref request) => request.url(),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(ref request) => request.url(),
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(ref request) => request.url(),
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(_) => None,
		}
	}

	/// Returns the peer's TLS-validated identity, if it presented a client
	/// certificate during the handshake that chained to a configured
	/// [`ServerTlsConfig::root`].
	///
	/// Only the Quinn backend supports mTLS; other backends always return `Ok(None)`.
	pub fn peer_identity(&self) -> anyhow::Result<Option<PeerIdentity>> {
		match self.kind {
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(ref request) => request.peer_identity(),
			#[cfg(feature = "noq")]
			RequestKind::Noq(_) => Ok(None),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(_) => Ok(None),
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(_) => Ok(None),
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(_) => Ok(None),
			#[cfg(not(any(
				feature = "noq",
				feature = "quinn",
				feature = "quiche",
				feature = "iroh",
				feature = "websocket"
			)))]
			_ => Ok(None),
		}
	}
}

/// TLS certificate information including fingerprints.
#[derive(Debug)]
pub struct ServerTlsInfo {
	#[cfg(any(feature = "noq", feature = "quinn"))]
	pub(crate) certs: Vec<Arc<rustls::sign::CertifiedKey>>,
	pub fingerprints: Vec<String>,
}

/// Server ID for QUIC-LB support.
#[serde_with::serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerId(#[serde_as(as = "serde_with::hex::Hex")] pub(crate) Vec<u8>);

impl ServerId {
	#[allow(dead_code)]
	pub(crate) fn len(&self) -> usize {
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
