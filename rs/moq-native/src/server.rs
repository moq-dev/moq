use std::net;
#[cfg(any(test, all(feature = "uds", unix)))]
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
	/// Listen for QUIC (UDP) on the given address. Defaults to `[::]:443`.
	///
	/// Accepts standard socket address syntax (e.g. `[::]:443`) or a DNS
	/// `host:port` pair (e.g. `fly-global-services:443`), resolved at bind time
	/// (first address only; Quinn cannot bind multiple). Leave unset while a
	/// `tcp`/`unix` listener is configured to run a stream-only server with no
	/// QUIC.
	#[serde(alias = "listen")]
	#[arg(id = "server-bind", long = "server-bind", alias = "listen", env = "MOQ_SERVER_BIND")]
	pub bind: Option<String>,

	/// Plaintext qmux TCP listener (`--server-tcp-bind`, no TLS). Requires the
	/// `tcp` feature.
	#[cfg(feature = "tcp")]
	#[command(flatten)]
	#[serde(default)]
	pub tcp: TcpConfig,

	/// Plaintext qmux Unix-socket listener (`--server-unix-bind`) with an optional
	/// peer-credential allowlist. Requires the `uds` feature; unix-only.
	#[cfg(all(feature = "uds", unix))]
	#[command(flatten)]
	#[serde(default)]
	pub unix: UnixConfig,

	/// The QUIC backend to use.
	/// Auto-detected from compiled features if not specified.
	#[arg(id = "server-backend", long = "server-backend", env = "MOQ_SERVER_BACKEND")]
	pub backend: Option<QuicBackend>,

	/// QUIC transport tuning (`--server-quic-*`): stream limits, GSO, timeouts,
	/// plus the accept-side knobs (preferred address, QUIC-LB connection IDs).
	#[command(flatten)]
	#[serde(default)]
	pub quic: crate::quic::Server,

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

/// Plaintext-TCP qmux listener settings (no TLS, no UDP).
///
/// TCP carries no peer identity, so it must only be reachable from trusted
/// clients. Bind it to loopback or a private interface; a non-loopback bind
/// logs a warning but is allowed.
#[cfg(feature = "tcp")]
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct TcpConfig {
	/// Bind a plaintext qmux TCP listener on this address.
	#[arg(long = "server-tcp-bind", id = "server-tcp-bind", env = "MOQ_SERVER_TCP_BIND")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub bind: Option<net::SocketAddr>,
}

/// Plaintext Unix-socket qmux listener settings, with an optional
/// peer-credential allowlist.
#[cfg(all(feature = "uds", unix))]
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct UnixConfig {
	/// Bind a plaintext qmux Unix-socket listener at this path.
	#[arg(long = "server-unix-bind", id = "server-unix-bind", env = "MOQ_SERVER_UNIX_BIND")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub bind: Option<PathBuf>,

	/// Peer-credential allowlist. `None` (the default) enforces nothing, so the
	/// socket's filesystem permissions are the only gate.
	#[command(flatten)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub allow: Option<UnixAllow>,
}

/// Peer-credential allowlist for a `unix://` listener.
///
/// The kernel reports the connecting process's credentials. Each populated list
/// constrains the corresponding credential (AND across the three, OR within
/// each); all empty means no check.
#[cfg(all(feature = "uds", unix))]
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct UnixAllow {
	/// Allowed peer user IDs. Empty means any uid.
	#[arg(
		long = "server-unix-allow-uid",
		env = "MOQ_SERVER_UNIX_ALLOW_UID",
		value_delimiter = ','
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub uid: Vec<u32>,

	/// Allowed peer group IDs. Empty means any gid.
	#[arg(
		long = "server-unix-allow-gid",
		env = "MOQ_SERVER_UNIX_ALLOW_GID",
		value_delimiter = ','
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub gid: Vec<u32>,

	/// Allowed peer PIDs. Empty means any pid; a populated list rejects peers
	/// whose PID the platform doesn't report.
	#[arg(
		long = "server-unix-allow-pid",
		env = "MOQ_SERVER_UNIX_ALLOW_PID",
		value_delimiter = ','
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pid: Vec<i32>,
}

#[cfg(all(feature = "uds", unix))]
impl UnixAllow {
	/// Whether any field is populated (i.e. the allowlist enforces something).
	fn is_empty(&self) -> bool {
		self.uid.is_empty() && self.gid.is_empty() && self.pid.is_empty()
	}

	/// Whether `cred` satisfies every populated field (AND across fields, OR
	/// within a field). A required pid is unsatisfiable when the platform
	/// reports none.
	fn permits(&self, cred: &crate::unix::PeerCred) -> bool {
		let uid_ok = self.uid.is_empty() || self.uid.contains(&cred.uid);
		let gid_ok = self.gid.is_empty() || self.gid.contains(&cred.gid);
		let pid_ok = self.pid.is_empty() || cred.pid.is_some_and(|pid| self.pid.contains(&pid));
		uid_ok && gid_ok && pid_ok
	}
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

	/// Whether a `tcp`/`unix` stream listener is configured.
	///
	/// When true and [`bind`](Self::bind) is unset, the server runs stream-only
	/// (no default QUIC listener).
	#[allow(unused_mut)]
	fn has_stream_listener(&self) -> bool {
		let mut has = false;
		#[cfg(feature = "tcp")]
		{
			has |= self.tcp.bind.is_some();
		}
		#[cfg(all(feature = "uds", unix))]
		{
			has |= self.unix.bind.is_some();
		}
		has
	}
}

/// Default bind address used when [`ServerConfig::bind`] is not set.
pub(crate) const DEFAULT_BIND: &str = "[::]:443";

/// Server for accepting MoQ connections.
///
/// Accepts QUIC (and optionally WebSocket), plus plaintext qmux over TCP
/// (`--server-tcp-bind`) and Unix sockets (`--server-unix-bind`). Create via
/// [`ServerConfig::init`] or [`Server::new`].
pub struct Server {
	moq: moq_net::Server,
	versions: moq_net::Versions,
	accept: FuturesUnordered<BoxFuture<'static, crate::Result<Request>>>,
	#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
	streams: StreamListeners,
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
			#[cfg(feature = "noq")]
			{
				QuicBackend::Noq
			}
			#[cfg(all(feature = "quinn", not(feature = "noq")))]
			{
				QuicBackend::Quinn
			}
			#[cfg(all(feature = "quiche", not(feature = "noq"), not(feature = "quinn")))]
			{
				QuicBackend::Quiche
			}
			#[cfg(all(not(feature = "quiche"), not(feature = "noq"), not(feature = "quinn")))]
			panic!("no QUIC backend compiled; enable noq, quinn, or quiche feature");
		});

		let versions = config.versions();

		// Build a QUIC backend when `--server-bind` is set, or when nothing else
		// is (the default). A stream-only server (`--server-unix-bind` with no
		// `--server-bind`) doesn't also open UDP/443.
		let build_quic = config.bind.is_some() || !config.has_stream_listener();

		if build_quic && !config.tls.root.is_empty() {
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
			QuicBackend::Noq if build_quic => Some(crate::noq::NoqServer::new(config.clone())?),
			_ => None,
		};

		#[cfg(feature = "quinn")]
		#[allow(unreachable_patterns)]
		let quinn = match backend {
			QuicBackend::Quinn if build_quic => Some(crate::quinn::QuinnServer::new(config.clone())?),
			_ => None,
		};

		#[cfg(feature = "quiche")]
		let quiche = match backend {
			QuicBackend::Quiche if build_quic => Some(crate::quiche::QuicheServer::new(config.clone())?),
			_ => None,
		};

		// Collect the configured stream listeners (at most one TCP, one Unix).
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		let mut stream_binds = Vec::new();
		#[cfg(feature = "tcp")]
		if let Some(addr) = config.tcp.bind {
			stream_binds.push(StreamBind::Tcp(addr));
		}
		#[cfg(all(feature = "uds", unix))]
		if let Some(path) = config.unix.bind.clone() {
			stream_binds.push(StreamBind::Unix(path));
		}
		// `None` (or an all-empty allowlist) means the listener enforces nothing.
		#[cfg(all(feature = "uds", unix))]
		let unix_allow = config.unix.allow.clone().filter(|allow| !allow.is_empty());
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		let streams = StreamListeners::new(
			stream_binds,
			stream_versions(&versions),
			#[cfg(all(feature = "uds", unix))]
			unix_allow,
		);

		Ok(Server {
			accept: Default::default(),
			moq: moq_net::Server::new().with_versions(versions.clone()),
			versions,
			#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
			streams,
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

	pub fn with_publisher(mut self, publish: impl moq_net::Consume<moq_net::origin::Consumer>) -> Self {
		self.moq = self.moq.with_publisher(publish);
		self
	}

	pub fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		self.moq = self.moq.with_subscriber(subscribe);
		self
	}

	#[doc(hidden)]
	#[deprecated(note = "renamed to `with_publisher`")]
	pub fn with_publish(self, publish: moq_net::origin::Consumer) -> Self {
		self.with_publisher(publish)
	}

	#[doc(hidden)]
	#[deprecated(note = "renamed to `with_subscriber`")]
	pub fn with_consume(self, subscribe: moq_net::origin::Producer) -> Self {
		self.with_subscriber(subscribe)
	}

	/// Attach a tier-scoped [`moq_net::StatsHandle`] to all sessions accepted by this server.
	pub fn with_stats(mut self, stats: moq_net::StatsHandle) -> Self {
		self.moq = self.moq.with_stats(stats);
		self
	}

	/// Accept sessions until the listener stops, serving `origin` to each subscriber.
	///
	/// Spawns a task per session and logs (rather than propagates) per-session
	/// errors, so one bad peer never tears down the listener. Returns when
	/// interrupted (Ctrl-C) or on a fatal bind failure. For per-session auth or
	/// routing, drive [`accept`](Self::accept) yourself instead.
	pub async fn serve_publish(self, origin: moq_net::origin::Consumer) -> crate::Result<()> {
		self.with_publisher(origin).serve().await
	}

	/// Accept sessions until the listener stops, ingesting each publisher into `origin`.
	///
	/// The mirror of [`serve_publish`](Self::serve_publish) for the consume direction.
	pub async fn serve_consume(self, origin: moq_net::origin::Producer) -> crate::Result<()> {
		self.with_subscriber(origin).serve().await
	}

	/// Shared accept loop for [`serve_publish`](Self::serve_publish) /
	/// [`serve_consume`](Self::serve_consume); the origin is already attached.
	async fn serve(mut self) -> crate::Result<()> {
		if let Ok(addr) = self.local_addr() {
			tracing::info!(%addr, "listening");
		}
		while let Some(request) = self.accept().await {
			tokio::spawn(async move {
				if let Err(err) = serve_session(request).await {
					tracing::warn!(%err, "session ended with error");
				}
			});
		}
		Ok(())
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
		// No QUIC backend (e.g. a stream-only `--server-bind`): no certificates.
		Arc::new(RwLock::new(crate::tls::Info::empty()))
	}

	#[cfg(not(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "tcp",
		all(feature = "uds", unix)
	)))]
	pub async fn accept(&mut self) -> Option<Request> {
		unreachable!("no transport compiled; enable a QUIC backend, tcp, or uds feature");
	}

	/// Returns the next partially established session, across every configured
	/// transport (QUIC, WebSocket, and plaintext qmux over TCP/Unix).
	///
	/// This returns a [Request] instead of a session so the connection can be
	/// rejected early on an invalid path or missing auth. Call [Request::ok] or
	/// [Request::close] to complete the handshake.
	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "tcp",
		all(feature = "uds", unix)
	))]
	pub async fn accept(&mut self) -> Option<Request> {
		// Bind the stream (tcp/unix) listeners on first poll; a bind failure is
		// fatal, mirroring how a QUIC bind failure aborts startup.
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		if let Err(err) = self.streams.ensure_started().await {
			tracing::error!(%err, "failed to bind stream listener");
			return None;
		}

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

			#[allow(unused_variables)]
			let server = self.moq.clone();
			#[allow(unused_variables)]
			let versions = self.versions.clone();

			// No streams configured: never resolves, so it doesn't disturb select!.
			#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
			let stream_accept = self.streams.recv();
			#[cfg(not(any(feature = "tcp", all(feature = "uds", unix))))]
			let stream_accept = std::future::pending::<Option<Request>>();

			tokio::select! {
				Some(request) = stream_accept => {
					return Some(request);
				}
				Some(_conn) = noq_accept => {
					#[cfg(feature = "noq")]
					{
						let alpns = versions.alpns();
						self.accept.push(async move {
							// Accept the transport (capturing url + mTLS identity) and exchange the
							// MoQ SETUP up front, so path/role are known before the caller authorizes
							// (like the stream bindings).
							let (session, url, identity) = super::noq::accept(_conn, alpns).await?;
							let request = server.accept_request(session).await?;
							Ok(Request { transport: Transport::Quic, url, identity, kind: RequestKind::Noq(Box::new(request)) })
						}.boxed());
					}
				}
				Some(_conn) = quinn_accept => {
					#[cfg(feature = "quinn")]
					{
						let alpns = versions.alpns();
						self.accept.push(async move {
							let (session, url, identity) = super::quinn::accept(_conn, alpns).await?;
							let request = server.accept_request(session).await?;
							Ok(Request { transport: Transport::Quic, url, identity, kind: RequestKind::Quinn(Box::new(request)) })
						}.boxed());
					}
				}
				Some(_conn) = quiche_accept => {
					#[cfg(feature = "quiche")]
					{
						let alpns = versions.alpns();
						self.accept.push(async move {
							let (session, url, identity) = super::quiche::accept(_conn, alpns).await?;
							let request = server.accept_request(session).await?;
							Ok(Request { transport: Transport::Quic, url, identity, kind: RequestKind::Quiche(Box::new(request)) })
						}.boxed());
					}
				}
				Some(_conn) = iroh_accept => {
					#[cfg(feature = "iroh")]
					self.accept.push(async move {
						let (session, url, identity) = super::iroh::accept(_conn).await?;
						let request = server.accept_request(session).await?;
						Ok(Request { transport: Transport::Iroh, url, identity, kind: RequestKind::Iroh(Box::new(request)) })
					}.boxed());
				}
				Some(_res) = ws_accept => {
					#[cfg(feature = "websocket")]
					match _res {
						Ok(session) => {
							// Read the SETUP off the qmux session before handing it over, so a
							// slow peer doesn't stall the accept loop (spawned like the others).
							self.accept.push(async move {
								let request = server.accept_request(session).await?;
								Ok(Request { transport: Transport::WebSocket, url: None, identity: None, kind: RequestKind::Qmux(Box::new(request)) })
							}.boxed());
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
		// No QUIC backend (e.g. a stream-only `--server-bind`).
		Err(Error::NoBackend("no QUIC listener configured"))
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

/// Complete one accepted [`Request`] and wait for the session to close.
async fn serve_session(request: Request) -> crate::Result<()> {
	let session = request.ok().await?;
	session.closed().await?;
	Ok(())
}

/// The version set offered on stream (`tcp://`/`unix://`) listeners.
///
/// A URL-less transport carries the request path in the moq-lite-05 SETUP, so
/// lite-05 is offered on top of the configured versions even when a custom set
/// omits it. Older versions still work for clients that need no path.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
fn stream_versions(base: &moq_net::Versions) -> moq_net::Versions {
	let mut versions: Vec<moq_net::Version> = base.iter().copied().collect();
	if let Ok(lite05) = "moq-lite-05".parse::<moq_net::Version>() {
		if !versions.contains(&lite05) {
			versions.push(lite05);
		}
	}
	moq_net::Versions::from(versions)
}

/// A configured stream listener (`--server-tcp-bind` / `--server-unix-bind`).
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
enum StreamBind {
	#[cfg(feature = "tcp")]
	Tcp(net::SocketAddr),
	#[cfg(all(feature = "uds", unix))]
	Unix(PathBuf),
}

/// The stream (`tcp`/`unix`) listeners owned by a [`Server`].
///
/// Bound lazily on the first [`Server::accept`] (they need a runtime), after
/// which each runs an accept loop in its own task and feeds completed [`Request`]s
/// back over a channel. The tasks own their listeners and are aborted when the
/// `Server` (and thus this) is dropped, so bound sockets don't linger.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
struct StreamListeners {
	binds: Vec<StreamBind>,
	versions: moq_net::Versions,
	#[cfg(all(feature = "uds", unix))]
	unix_allow: Option<UnixAllow>,
	rx: Option<tokio::sync::mpsc::Receiver<Request>>,
	tasks: Vec<tokio::task::JoinHandle<()>>,
}

#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
impl StreamListeners {
	fn new(
		binds: Vec<StreamBind>,
		versions: moq_net::Versions,
		#[cfg(all(feature = "uds", unix))] unix_allow: Option<UnixAllow>,
	) -> Self {
		Self {
			binds,
			versions,
			#[cfg(all(feature = "uds", unix))]
			unix_allow,
			rx: None,
			tasks: Vec::new(),
		}
	}

	/// Bind the configured listeners and spawn their accept loops, once.
	async fn ensure_started(&mut self) -> crate::Result<()> {
		if self.rx.is_some() || self.binds.is_empty() {
			return Ok(());
		}

		let (tx, rx) = tokio::sync::mpsc::channel(16);
		for bind in self.binds.drain(..) {
			let versions = self.versions.clone();
			match bind {
				#[cfg(feature = "tcp")]
				StreamBind::Tcp(addr) => {
					if !addr.ip().is_loopback() {
						tracing::warn!(%addr, "tcp listener bound to a non-loopback address; qmux is UNENCRYPTED, ensure the network is trusted");
					}
					let listener = crate::tcp::Listener::bind(addr).await?.with_protocols(versions.alpns());
					tracing::info!(%addr, "listening (tcp)");
					self.tasks.push(spawn_tcp_loop(listener, versions, tx.clone()));
				}
				#[cfg(all(feature = "uds", unix))]
				StreamBind::Unix(path) => {
					let listener = crate::unix::Listener::bind(&path)
						.await?
						.with_protocols(versions.alpns());
					// Loose file perms: the uid/gid/pid allow list is the real gate,
					// and the worker usually runs as a different user than the server.
					listener.set_mode(0o666)?;
					tracing::info!(path = %path.display(), allow = ?self.unix_allow, "listening (unix)");
					self.tasks
						.push(spawn_unix_loop(listener, versions, self.unix_allow.clone(), tx.clone()));
				}
			}
		}

		self.rx = Some(rx);
		Ok(())
	}

	/// Yield the next stream [`Request`], or pend forever if none are running.
	async fn recv(&mut self) -> Option<Request> {
		match self.rx.as_mut() {
			Some(rx) => rx.recv().await,
			None => std::future::pending().await,
		}
	}
}

#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
impl Drop for StreamListeners {
	fn drop(&mut self) {
		// Stop the accept loops so their listeners (and bound sockets) are freed.
		for task in &self.tasks {
			task.abort();
		}
	}
}

#[cfg(feature = "tcp")]
fn spawn_tcp_loop(
	listener: crate::tcp::Listener,
	versions: moq_net::Versions,
	tx: tokio::sync::mpsc::Sender<Request>,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		loop {
			match listener.accept().await {
				Some(Ok(session)) => spawn_stream_request(session, Transport::Tcp, versions.clone(), tx.clone()),
				Some(Err(err)) => tracing::warn!(%err, "tcp listener accept failed"),
				None => break,
			}
		}
	})
}

#[cfg(all(feature = "uds", unix))]
fn spawn_unix_loop(
	listener: crate::unix::Listener,
	versions: moq_net::Versions,
	allow: Option<UnixAllow>,
	tx: tokio::sync::mpsc::Sender<Request>,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		loop {
			match listener.accept().await {
				Some(Ok((session, cred))) => {
					// Enforce the allowlist (if any) before reading SETUP bytes from the peer.
					if let Some(allow) = &allow
						&& !allow.permits(&cred)
					{
						tracing::warn!(uid = cred.uid, gid = cred.gid, pid = ?cred.pid, "unix connection rejected by allow list");
						continue;
					}
					spawn_stream_request(session, Transport::Unix, versions.clone(), tx.clone());
				}
				Some(Err(err)) => tracing::warn!(%err, "unix listener accept failed"),
				None => break,
			}
		}
	})
}

/// Read the SETUP from an accepted stream session (concurrently, so one slow or
/// malicious peer doesn't stall the listener) and forward the resulting request.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
fn spawn_stream_request(
	session: qmux::Session,
	transport: Transport,
	versions: moq_net::Versions,
	tx: tokio::sync::mpsc::Sender<Request>,
) {
	tokio::spawn(async move {
		let server = moq_net::Server::new().with_versions(versions);
		match server.accept_request(session).await {
			Ok(request) => {
				let request = Request {
					transport,
					url: None,
					identity: None,
					kind: RequestKind::Qmux(Box::new(request)),
				};
				let _ = tx.send(request).await;
			}
			Err(err) => tracing::debug!(%err, "stream SETUP handshake failed"),
		}
	});
}

/// An accepted connection whose MoQ SETUP has already been exchanged.
///
/// Every backend drives the transport connect *and* the MoQ handshake up front, so the
/// [`path`](Request::path)/[`role`](Request::role) a client advertised are available on
/// every transport before the caller authorizes. The variant only distinguishes the
/// underlying session type; all of them delegate identically.
pub(crate) enum RequestKind {
	#[cfg(feature = "noq")]
	Noq(Box<moq_net::Request<web_transport_noq::Session>>),
	#[cfg(feature = "quinn")]
	Quinn(Box<moq_net::Request<web_transport_quinn::Session>>),
	#[cfg(feature = "quiche")]
	Quiche(Box<moq_net::Request<web_transport_quiche::Connection>>),
	#[cfg(feature = "iroh")]
	Iroh(Box<moq_net::Request<web_transport_iroh::Session>>),
	#[cfg(any(feature = "tcp", all(feature = "uds", unix), feature = "websocket"))]
	Qmux(Box<moq_net::Request<qmux::Session>>),
}

/// The network transport carrying an incoming MoQ session.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Transport {
	/// QUIC, either directly or through WebTransport over HTTP/3.
	Quic,
	/// An Iroh QUIC connection.
	Iroh,
	/// A WebSocket connection using qmux framing.
	WebSocket,
	/// A plaintext TCP connection using qmux framing.
	Tcp,
	/// A Unix domain socket using qmux framing.
	Unix,
}

impl Transport {
	/// Returns the stable lowercase name used in logs and external metadata.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Quic => "quic",
			Self::Iroh => "iroh",
			Self::WebSocket => "websocket",
			Self::Tcp => "tcp",
			Self::Unix => "unix",
		}
	}
}

impl std::fmt::Display for Transport {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

/// An incoming MoQ session that can be accepted or rejected.
///
/// The transport connection and the MoQ SETUP are already complete, so [`path`](Self::path),
/// [`role`](Self::role), [`url`](Self::url), and [`peer_identity`](Self::peer_identity) are
/// all populated consistently regardless of transport. [Self::with_publisher] and
/// [Self::with_subscriber] configure what is published and subscribed to on the session;
/// otherwise the Server's configuration is used by default. Call [Self::ok] to start the
/// session, or [Self::close] to reject it (which closes the just-established session).
pub struct Request {
	transport: Transport,
	/// The dial URL, for transports that carry one (QUIC/WebTransport). `None` for the
	/// URL-less stream bindings, whose request path rides the SETUP instead.
	url: Option<Url>,
	/// The peer's validated mTLS identity, captured at the transport handshake (before
	/// the MoQ SETUP), when the backend supports it.
	identity: Option<crate::tls::PeerIdentity>,
	kind: RequestKind,
}

/// Delegate a read-only call to the inner [`moq_net::Request`], whatever the transport.
macro_rules! request_ref {
	($self:expr, $r:ident => $body:expr) => {
		match &$self.kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq($r) => $body,
			#[cfg(feature = "quinn")]
			RequestKind::Quinn($r) => $body,
			#[cfg(feature = "quiche")]
			RequestKind::Quiche($r) => $body,
			#[cfg(feature = "iroh")]
			RequestKind::Iroh($r) => $body,
			#[cfg(any(feature = "tcp", all(feature = "uds", unix), feature = "websocket"))]
			RequestKind::Qmux($r) => $body,
		}
	};
}

/// Delegate a consuming call whose arms all yield the same type (e.g. `ok`, `close`).
macro_rules! request_into {
	($kind:expr, $r:ident => $body:expr) => {
		match $kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq($r) => $body,
			#[cfg(feature = "quinn")]
			RequestKind::Quinn($r) => $body,
			#[cfg(feature = "quiche")]
			RequestKind::Quiche($r) => $body,
			#[cfg(feature = "iroh")]
			RequestKind::Iroh($r) => $body,
			#[cfg(any(feature = "tcp", all(feature = "uds", unix), feature = "websocket"))]
			RequestKind::Qmux($r) => $body,
		}
	};
}

/// Delegate a consuming builder call, rebuilding the same variant from the returned request.
macro_rules! request_map {
	($kind:expr, $r:ident => $body:expr) => {
		match $kind {
			#[cfg(feature = "noq")]
			RequestKind::Noq($r) => RequestKind::Noq(Box::new($body)),
			#[cfg(feature = "quinn")]
			RequestKind::Quinn($r) => RequestKind::Quinn(Box::new($body)),
			#[cfg(feature = "quiche")]
			RequestKind::Quiche($r) => RequestKind::Quiche(Box::new($body)),
			#[cfg(feature = "iroh")]
			RequestKind::Iroh($r) => RequestKind::Iroh(Box::new($body)),
			#[cfg(any(feature = "tcp", all(feature = "uds", unix), feature = "websocket"))]
			RequestKind::Qmux($r) => RequestKind::Qmux(Box::new($body)),
		}
	};
}

impl Request {
	/// Reject the session. The transport is already accepted, so this closes the
	/// just-established MoQ session rather than answering the transport handshake:
	/// the `code` (an HTTP-style status the caller passes) maps to a MoQ close reason.
	pub async fn close(self, code: u16) -> crate::Result<()> {
		let err = match code {
			401 | 403 => moq_net::Error::Unauthorized,
			other => moq_net::Error::App(other),
		};
		request_into!(self.kind, request => request.close(err));
		Ok(())
	}

	/// Publish the given origin to the session.
	pub fn with_publisher(self, publish: impl moq_net::Consume<moq_net::origin::Consumer>) -> Self {
		let Request {
			transport,
			url,
			identity,
			kind,
		} = self;
		let kind = request_map!(kind, request => request.with_publisher(publish));
		Request {
			transport,
			url,
			identity,
			kind,
		}
	}

	/// Subscribe to the given origin from the session.
	pub fn with_subscriber(self, subscribe: moq_net::origin::Producer) -> Self {
		let Request {
			transport,
			url,
			identity,
			kind,
		} = self;
		let kind = request_map!(kind, request => request.with_subscriber(subscribe));
		Request {
			transport,
			url,
			identity,
			kind,
		}
	}

	#[doc(hidden)]
	#[deprecated(note = "renamed to `with_publisher`")]
	pub fn with_publish(self, publish: moq_net::origin::Consumer) -> Self {
		self.with_publisher(publish)
	}

	#[doc(hidden)]
	#[deprecated(note = "renamed to `with_subscriber`")]
	pub fn with_consume(self, subscribe: moq_net::origin::Producer) -> Self {
		self.with_subscriber(subscribe)
	}

	/// Attach a tier-scoped [`moq_net::StatsHandle`] to this session.
	pub fn with_stats(self, stats: moq_net::StatsHandle) -> Self {
		let Request {
			transport,
			url,
			identity,
			kind,
		} = self;
		let kind = request_map!(kind, request => request.with_stats(stats));
		Request {
			transport,
			url,
			identity,
			kind,
		}
	}

	/// Accept the session, starting the MoQ session loops.
	pub async fn ok(self) -> crate::Result<Session> {
		Ok(request_into!(self.kind, request => request.ok().await?))
	}

	/// Returns the network transport carrying this session.
	pub fn transport(&self) -> Transport {
		self.transport
	}

	/// Returns the URL the client dialed, for transports that carry one (QUIC/WebTransport).
	///
	/// `None` for the URL-less stream bindings (`tcp`/`unix`); use [`Self::path`] for their
	/// in-band request path.
	pub fn url(&self) -> Option<&Url> {
		self.url.as_ref()
	}

	/// The request path the client advertised, uniform across transports.
	///
	/// Taken from the SETUP for the URL-less stream bindings (and moq-transport, which
	/// carries it in-band), and from the dial [`url`](Self::url) for WebTransport/QUIC.
	/// `None` only when neither carries one.
	pub fn path(&self) -> Option<&str> {
		let setup = request_ref!(self, r => r.path());
		setup.or(self.url.as_ref().map(Url::path))
	}

	/// The direction the client advertised in its SETUP ([`moq_net::Role::Both`] when it
	/// omitted one), available on every transport. Use it to reject a token that lacks the
	/// scope for the client's intended direction.
	pub fn role(&self) -> moq_net::Role {
		request_ref!(self, r => r.role())
	}

	/// The client certificate chain the peer presented, if any, validated
	/// against a configured [`crate::tls::Server::root`] during the handshake.
	///
	/// Captured at the transport handshake (before the SETUP). Only the Quinn and noq
	/// backends support mTLS; other transports always return `None`. Use it to grant
	/// elevated access or to close the session once the certificate expires (see
	/// [`crate::tls::PeerIdentity::expiry`]).
	pub fn peer_identity(&self) -> Option<crate::tls::PeerIdentity> {
		self.identity.clone()
	}

	/// Whether the peer presented a valid client certificate during the handshake.
	#[deprecated(note = "use `peer_identity` instead")]
	pub fn has_peer_certificate(&self) -> bool {
		self.peer_identity().is_some()
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
	fn transport_names_are_stable() {
		assert_eq!(Transport::Quic.as_str(), "quic");
		assert_eq!(Transport::Iroh.as_str(), "iroh");
		assert_eq!(Transport::WebSocket.as_str(), "websocket");
		assert_eq!(Transport::Tcp.as_str(), "tcp");
		assert_eq!(Transport::Unix.as_str(), "unix");
	}

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

	#[test]
	fn bind_string_or_listen_alias() {
		// The QUIC bind is a plain address; the `listen` alias still works.
		let bind: ServerConfig = toml::from_str(r#"bind = "[::]:443""#).unwrap();
		assert_eq!(bind.bind.as_deref(), Some("[::]:443"));

		let alias: ServerConfig = toml::from_str(r#"listen = "0.0.0.0:4443""#).unwrap();
		assert_eq!(alias.bind.as_deref(), Some("0.0.0.0:4443"));
	}

	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn stream_listener_config_parses() {
		let config: ServerConfig = toml::from_str(
			r#"
bind = "[::]:443"

[unix]
bind = "/run/moq.sock"

[unix.allow]
uid = [1001, 1002]
"#,
		)
		.unwrap();
		assert_eq!(config.bind.as_deref(), Some("[::]:443"));
		assert_eq!(config.unix.bind.as_deref(), Some(std::path::Path::new("/run/moq.sock")));
		assert_eq!(config.unix.allow.as_ref().expect("allow").uid, vec![1001, 1002]);
		assert!(config.has_stream_listener());
	}

	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn stream_only_config_has_no_quic() {
		// A unix listener with no `--server-bind` is stream-only.
		let mut config = ServerConfig::default();
		config.unix.bind = Some(PathBuf::from("/run/moq.sock"));
		assert!(config.has_stream_listener());
		assert!(config.bind.is_none());

		// The default (nothing configured) still runs QUIC.
		assert!(!ServerConfig::default().has_stream_listener());
	}
}
