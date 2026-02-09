use std::path::PathBuf;
use std::{net, time::Duration};

use crate::QuicBackend;
#[cfg(feature = "iroh")]
use crate::iroh::IrohQuicRequest;
use anyhow::Context;
use moq_lite::Session;
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

	#[command(flatten)]
	#[serde(default)]
	pub tls: ServerTlsConfig,
}

impl ServerConfig {
	pub fn init(self) -> anyhow::Result<Server> {
		let backend = self.backend.clone().unwrap_or_else(|| {
			if cfg!(feature = "quinn") {
				QuicBackend::Quinn
			} else if cfg!(feature = "quiche") {
				QuicBackend::Quiche
			} else {
				panic!("no QUIC backend compiled; enable quinn or quiche feature")
			}
		});

		let inner = match backend {
			QuicBackend::Quinn => {
				#[cfg(not(feature = "quinn"))]
				anyhow::bail!("quinn backend not compiled; rebuild with --features quinn");

				#[cfg(feature = "quinn")]
				ServerInner::Quinn(crate::quinn::QuinnServer::new(self)?)
			}
			QuicBackend::Quiche => {
				#[cfg(not(feature = "quiche"))]
				anyhow::bail!("quiche backend not compiled; rebuild with --features quiche");

				#[cfg(feature = "quiche")]
				ServerInner::Quiche(crate::quiche::QuicheServer::new(self)?)
			}
		};

		Ok(Server {
			inner,
			accept: Default::default(),
			moq: moq_lite::Server::new(),
			#[cfg(feature = "iroh")]
			iroh: None,
		})
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
	Quinn(crate::quinn::QuinnServer),
	#[cfg(feature = "quiche")]
	Quiche(crate::quiche::QuicheServer),
}

impl Server {
	/// Create a new server using the default (quinn) backend.
	#[cfg(feature = "quinn")]
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		config.init()
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
		match &self.inner {
			#[cfg(feature = "quinn")]
			ServerInner::Quinn(quinn) => quinn.tls_info(),
			#[cfg(feature = "quiche")]
			ServerInner::Quiche(quiche) => quiche.tls_info(),
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
				ServerInner::Quinn(quinn) => {
					tokio::select! {
						res = quinn.accept() => {
							let conn = res?;
							self.accept.push(crate::quinn::accept_quinn_session(self.moq.clone(), conn).boxed());
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
				ServerInner::Quiche(quiche) => {
					tokio::select! {
						res = quiche.accept() => {
							let conn = res?;
							self.accept.push(crate::quiche::accept_quiche_session(self.moq.clone(), conn).boxed());
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
			ServerInner::Quinn(quinn) => quinn.local_addr(),
			#[cfg(feature = "quiche")]
			ServerInner::Quiche(quiche) => quiche.local_addr(),
		}
	}

	pub fn close(&mut self) {
		match &mut self.inner {
			#[cfg(feature = "quinn")]
			ServerInner::Quinn(quinn) => quinn.close(),
			#[cfg(feature = "quiche")]
			ServerInner::Quiche(quiche) => quiche.close(),
		}
	}
}

/// An incoming connection that can be accepted or rejected.
pub(crate) enum RequestKind {
	#[cfg(feature = "quinn")]
	QuinnWebTransport(web_transport_quinn::Request),
	#[cfg(feature = "quinn")]
	QuinnQuic(crate::quinn::QuinnRequest),
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
	pub(crate) server: moq_lite::Server,
	pub(crate) kind: RequestKind,
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
