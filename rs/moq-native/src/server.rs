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

	#[command(flatten)]
	#[serde(default)]
	pub tls: ServerTlsConfig,
}

impl ServerConfig {
	pub fn init(self) -> anyhow::Result<Server> {
		Server::new(self)
	}
}

/// Server for accepting MoQ connections over QUIC.
///
/// Create via [`ServerConfig::init`] or [`Server::new`].
pub struct Server {
	moq: moq_lite::Server,
	accept: FuturesUnordered<BoxFuture<'static, anyhow::Result<Request>>>,
	#[cfg(feature = "iroh")]
	iroh: Option<iroh::Endpoint>,
	#[cfg(feature = "quinn")]
	quinn: Option<crate::quinn::QuinnServer>,
	#[cfg(feature = "quiche")]
	quiche: Option<crate::quiche::QuicheServer>,
}

impl Server {
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
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
			moq: moq_lite::Server::new(),
			#[cfg(feature = "iroh")]
			iroh: None,
			#[cfg(feature = "quinn")]
			quinn,
			#[cfg(feature = "quiche")]
			quiche,
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

	// Return the SHA256 fingerprints of all our certificates.
	pub fn tls_info(&self) -> Arc<RwLock<ServerTlsInfo>> {
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

	#[cfg(not(any(feature = "quinn", feature = "quiche", feature = "iroh")))]
	pub async fn accept(&mut self) -> Option<Request> {
		unreachable!("no QUIC backend compiled; enable quinn, quiche, or iroh feature");
	}

	/// Returns the next partially established QUIC or WebTransport session.
	///
	/// This returns a [Request] instead of a [web_transport_quinn::Session]
	/// so the connection can be rejected early on an invalid path or missing auth.
	///
	/// The [Request] is either a WebTransport or a raw QUIC request.
	/// Call [Request::ok] or [Request::close] to complete the handshake.
	#[cfg(any(feature = "quinn", feature = "quiche", feature = "iroh"))]
	pub async fn accept(&mut self) -> Option<Request> {
		loop {
			// tokio::select! does not support cfg directives on arms, so we need to create the futures here.
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

			let server = self.moq.clone();

			tokio::select! {
				Some(conn) = quinn_accept => {
					#[cfg(feature = "quinn")]
					self.accept.push(async move {
						let quinn = super::quinn::QuinnRequest::accept(conn).await?;
						Ok(Request {
							server,
							kind: RequestKind::Quinn(quinn),
						})
					}.boxed());
				}
				Some(_conn) = quiche_accept => {
					#[cfg(feature = "quiche")]
					self.accept.push(async move {
						let quiche = super::quiche::QuicheRequest::accept(_conn).await?;
						Ok(Request {
							server,
							kind: RequestKind::Quiche(quiche),
						})
					}.boxed());
				}
				Some(conn) = iroh_accept => {
					#[cfg(feature = "iroh")]
					self.accept.push(async move {
						let iroh = super::iroh::IrohRequest::accept(conn).await?;
						Ok(Request {
							server,
							kind: RequestKind::Iroh(iroh),
						})
					}.boxed());
				}
				Some(res) = self.accept.next() => {
					match res {
						Ok(session) => return Some(session),
						Err(err) => tracing::debug!(%err, "failed to accept session"),
					}
				}
				_ = tokio::signal::ctrl_c() => {
					self.close();
					tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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

	pub fn close(&mut self) {
		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_mut() {
			quinn.close();
		}
		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_mut() {
			quiche.close();
		}
		unreachable!("no QUIC backend compiled");
	}
}

/// An incoming connection that can be accepted or rejected.
pub(crate) enum RequestKind {
	#[cfg(feature = "quinn")]
	Quinn(crate::quinn::QuinnRequest),
	#[cfg(feature = "quiche")]
	Quiche(crate::quiche::QuicheRequest),
	#[cfg(feature = "iroh")]
	Iroh(crate::iroh::IrohRequest),
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
		}
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> Option<&Url> {
		#[cfg(not(any(feature = "quinn", feature = "quiche", feature = "iroh")))]
		unreachable!("no QUIC backend compiled; enable quinn, quiche, or iroh feature");

		match self.kind {
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(ref request) => request.url(),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(ref request) => request.url(),
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(ref request) => request.url(),
		}
	}
}

/// TLS certificate information including fingerprints.
#[derive(Debug)]
pub struct ServerTlsInfo {
	#[cfg(feature = "quinn")]
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
