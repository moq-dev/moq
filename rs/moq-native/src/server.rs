use std::net;
#[cfg(test)]
use std::path::PathBuf;

use crate::{Error, QuicBackend};
use moq_net::Session;
use std::sync::{Arc, RwLock};
use url::Url;
#[cfg(feature = "iroh")]
use web_transport_iroh::iroh;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;

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

	/// IPv4 address advertised as the QUIC preferred_address.
	///
	/// Supporting clients (Chrome M131+, native Quinn) migrate to this address
	/// shortly after the handshake completes. Typical use: handshake on an
	/// anycast IP, steady-state on this host's unicast IP.
	///
	/// Honored by the Quinn and noq backends.
	#[arg(
		id = "server-preferred-v4",
		long = "server-preferred-v4",
		env = "MOQ_SERVER_PREFERRED_V4"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v4: Option<net::SocketAddrV4>,

	/// IPv6 address advertised as the QUIC preferred_address.
	///
	/// See [`Self::preferred_v4`].
	#[arg(
		id = "server-preferred-v6",
		long = "server-preferred-v6",
		env = "MOQ_SERVER_PREFERRED_V6"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v6: Option<net::SocketAddrV6>,

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
	pub version: Vec<moq_net::Version>,

	#[command(flatten)]
	#[serde(default)]
	pub tls: crate::tls::Server,
}

impl ServerConfig {
	pub fn init(self) -> crate::Result<Server> {
		Server::new(self)
	}

	/// Returns the configured versions, defaulting to all if none specified.
	pub fn versions(&self) -> moq_net::Versions {
		if self.version.is_empty() {
			moq_net::Versions::all()
		} else {
			moq_net::Versions::from(self.version.clone())
		}
	}
}

/// Default bind address used when [`ServerConfig::bind`] is not set.
pub(crate) const DEFAULT_BIND: &str = "[::]:443";

/// Server for accepting MoQ connections over QUIC.
///
/// Create via [`ServerConfig::init`] or [`Server::new`].
pub struct Server {
	moq: moq_net::Server,
	versions: moq_net::Versions,
	accept: FuturesUnordered<BoxFuture<'static, crate::Result<Request>>>,
	#[cfg(feature = "iroh")]
	iroh: Option<iroh::Endpoint>,
	#[cfg(feature = "noq")]
	noq: Option<crate::noq::NoqServer>,
	#[cfg(feature = "quinn")]
	quinn: Option<crate::quinn::QuinnServer>,
	#[cfg(feature = "quiche")]
	quiche: Option<crate::quiche::QuicheServer>,
	#[cfg(feature = "websocket")]
	websocket: Option<crate::websocket::Listener>,
}

impl Server {
	pub fn new(config: ServerConfig) -> crate::Result<Self> {
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
			let mtls_supported = match backend {
				#[cfg(feature = "quinn")]
				QuicBackend::Quinn => true,
				#[cfg(feature = "noq")]
				QuicBackend::Noq => true,
				#[allow(unreachable_patterns)]
				_ => false,
			};
			if !mtls_supported {
				return Err(Error::MtlsUnsupported);
			}
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
			moq: moq_net::Server::new().with_versions(versions.clone()),
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
	pub fn with_websocket(mut self, websocket: Option<crate::websocket::Listener>) -> Self {
		self.websocket = websocket;
		self
	}

	#[cfg(feature = "iroh")]
	pub fn with_iroh(mut self, iroh: Option<iroh::Endpoint>) -> Self {
		self.iroh = iroh;
		self
	}

	pub fn with_publisher(mut self, publish: impl moq_net::Consume<moq_net::OriginConsumer>) -> Self {
		self.moq = self.moq.with_publisher(publish);
		self
	}

	pub fn with_subscriber(mut self, subscribe: moq_net::OriginProducer) -> Self {
		self.moq = self.moq.with_subscriber(subscribe);
		self
	}

	/// Deprecated alias for [`with_publisher`](Self::with_publisher).
	#[deprecated(note = "renamed to `with_publisher`")]
	pub fn with_publish(self, publish: moq_net::OriginConsumer) -> Self {
		self.with_publisher(publish)
	}

	/// Deprecated alias for [`with_subscriber`](Self::with_subscriber).
	#[deprecated(note = "renamed to `with_subscriber`")]
	pub fn with_consume(self, subscribe: moq_net::OriginProducer) -> Self {
		self.with_subscriber(subscribe)
	}

	/// Attach a tier-scoped [`moq_net::StatsHandle`] to all sessions accepted by this server.
	pub fn with_stats(mut self, stats: moq_net::StatsHandle) -> Self {
		self.moq = self.moq.with_stats(stats);
		self
	}

	// Return the SHA256 fingerprints of all our certificates.
	pub fn tls_info(&self) -> Arc<RwLock<crate::tls::Info>> {
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
			let ws_accept = std::future::pending::<Option<crate::Result<()>>>();

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
						let iroh = super::iroh::Request::accept(_conn).await?;
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
								kind: RequestKind::WebSocket(Box::new(session)),
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

	pub fn local_addr(&self) -> crate::Result<net::SocketAddr> {
		#[cfg(feature = "noq")]
		if let Some(noq) = self.noq.as_ref() {
			return Ok(noq.local_addr()?);
		}
		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_ref() {
			return Ok(quinn.local_addr()?);
		}
		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_ref() {
			return Ok(quiche.local_addr()?);
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

/// An incoming connection that can be accepted or rejected.
pub(crate) enum RequestKind {
	#[cfg(feature = "noq")]
	Noq(crate::noq::NoqRequest),
	#[cfg(feature = "quinn")]
	Quinn(Box<crate::quinn::QuinnRequest>),
	#[cfg(feature = "quiche")]
	Quiche(crate::quiche::QuicheRequest),
	#[cfg(feature = "iroh")]
	Iroh(crate::iroh::Request),
	#[cfg(feature = "websocket")]
	WebSocket(Box<qmux::Session>),
}

/// An incoming MoQ session that can be accepted or rejected.
///
/// [Self::with_publisher] and [Self::with_subscriber] will configure what is published and subscribed to on the session respectively.
/// Otherwise, the Server's configuration is used by default.
pub struct Request {
	server: moq_net::Server,
	kind: RequestKind,
}

impl Request {
	/// Reject the session, returning your favorite HTTP status code.
	pub async fn close(self, _code: u16) -> crate::Result<()> {
		match self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq(request) => {
				let status =
					web_transport_noq::http::StatusCode::from_u16(_code).map_err(|_| Error::InvalidStatusCode)?;
				request.close(status).await.map_err(crate::noq::Error::Server)?;
				Ok(())
			}
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(request) => {
				let status =
					web_transport_quinn::http::StatusCode::from_u16(_code).map_err(|_| Error::InvalidStatusCode)?;
				request.close(status).await.map_err(crate::quinn::Error::Server)?;
				Ok(())
			}
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(request) => {
				let status =
					web_transport_quiche::http::StatusCode::from_u16(_code).map_err(|_| Error::InvalidStatusCode)?;
				request.reject(status).await.map_err(crate::quiche::Error::Reject)?;
				Ok(())
			}
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(request) => {
				let status =
					web_transport_iroh::http::StatusCode::from_u16(_code).map_err(|_| Error::InvalidStatusCode)?;
				request.close(status).await.map_err(crate::iroh::Error::Server)?;
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
	pub fn with_publisher(mut self, publish: impl moq_net::Consume<moq_net::OriginConsumer>) -> Self {
		self.server = self.server.with_publisher(publish);
		self
	}

	/// Subscribe to the given origin from the session.
	pub fn with_subscriber(mut self, subscribe: moq_net::OriginProducer) -> Self {
		self.server = self.server.with_subscriber(subscribe);
		self
	}

	/// Deprecated alias for [`with_publisher`](Self::with_publisher).
	#[deprecated(note = "renamed to `with_publisher`")]
	pub fn with_publish(self, publish: moq_net::OriginConsumer) -> Self {
		self.with_publisher(publish)
	}

	/// Deprecated alias for [`with_subscriber`](Self::with_subscriber).
	#[deprecated(note = "renamed to `with_subscriber`")]
	pub fn with_consume(self, subscribe: moq_net::OriginProducer) -> Self {
		self.with_subscriber(subscribe)
	}

	/// Attach a tier-scoped [`moq_net::StatsHandle`] to this session.
	pub fn with_stats(mut self, stats: moq_net::StatsHandle) -> Self {
		self.server = self.server.with_stats(stats);
		self
	}

	/// Accept the session, performing rest of the MoQ handshake.
	pub async fn ok(self) -> crate::Result<Session> {
		match self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq(request) => Ok(self
				.server
				.accept(request.ok().await.map_err(crate::noq::Error::Server)?)
				.await?),
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(request) => Ok(self
				.server
				.accept(request.ok().await.map_err(crate::quinn::Error::Server)?)
				.await?),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(request) => {
				let conn = request.ok().await.map_err(crate::quiche::Error::Accept)?;
				Ok(self.server.accept(conn).await?)
			}
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(request) => Ok(self
				.server
				.accept(request.ok().await.map_err(crate::iroh::Error::Server)?)
				.await?),
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(session) => Ok(self.server.accept(*session).await?),
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

	/// Whether the peer presented a client certificate during the handshake
	/// that chained to a configured [`crate::tls::Server::root`].
	///
	/// Only the Quinn and noq backends support mTLS; other backends always return `false`.
	pub fn has_peer_certificate(&self) -> bool {
		match self.kind {
			#[cfg(feature = "quinn")]
			RequestKind::Quinn(ref request) => request.has_peer_certificate(),
			#[cfg(feature = "noq")]
			RequestKind::Noq(ref request) => request.has_peer_certificate(),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche(_) => false,
			#[cfg(feature = "iroh")]
			RequestKind::Iroh(_) => false,
			#[cfg(feature = "websocket")]
			RequestKind::WebSocket(_) => false,
			#[cfg(not(any(
				feature = "noq",
				feature = "quinn",
				feature = "quiche",
				feature = "iroh",
				feature = "websocket"
			)))]
			_ => false,
		}
	}
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_tls_string_or_array() {
		// Single string should deserialize into a Vec with one entry.
		let single = r#"
			cert = "cert.pem"
			key = "key.pem"
		"#;
		let config: crate::tls::Server = toml::from_str(single).unwrap();
		assert_eq!(config.cert, vec![PathBuf::from("cert.pem")]);
		assert_eq!(config.key, vec![PathBuf::from("key.pem")]);

		// TOML arrays should still work.
		let array = r#"
			cert = ["a.pem", "b.pem"]
			key = ["a.key", "b.key"]
			generate = ["localhost"]
			root = ["ca.pem"]
		"#;
		let config: crate::tls::Server = toml::from_str(array).unwrap();
		assert_eq!(config.cert, vec![PathBuf::from("a.pem"), PathBuf::from("b.pem")]);
		assert_eq!(config.key, vec![PathBuf::from("a.key"), PathBuf::from("b.key")]);
		assert_eq!(config.generate, vec!["localhost".to_string()]);
		assert_eq!(config.root, vec![PathBuf::from("ca.pem")]);
	}
}
