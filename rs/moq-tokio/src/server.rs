use std::net;
#[cfg(any(test, all(feature = "uds", unix)))]
use std::path::PathBuf;

#[cfg(feature = "iroh")]
use crate::iroh;
use crate::{Error, QuicBackend};
use moq_net::Session;
use url::Url;

// Only the transports that finish their handshake in a spawned future need `.boxed()`;
// the stream listeners hand back an already-built `Request`.
#[cfg(any(
	feature = "noq",
	feature = "quinn",
	feature = "quiche",
	feature = "iroh",
	feature = "websocket"
))]
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;

impl crate::listen::Config {
	/// Build the [`Server`] this config describes, binding its listeners.
	pub fn init(self, quic: crate::quic::Config) -> crate::Result<Server> {
		Server::new(self, quic)
	}

	/// Build a server with only the `tcp`/`unix` listeners, leaving the QUIC
	/// (UDP) bind to someone else.
	///
	/// For a process whose QUIC lives on other threads: a
	/// [`worker::Workers`](crate::worker::Workers) group holds the UDP port from
	/// its own pinned threads while this server keeps the stream listeners on the
	/// caller's runtime. A server with no stream listener configured accepts
	/// nothing, which is the honest outcome rather than a surprise UDP bind.
	///
	/// Distinct from clearing [`bind`](crate::listen::Config::bind), which still
	/// opens the default QUIC listener when nothing else is configured.
	pub fn init_streams(self) -> crate::Result<Server> {
		Server::build(self, crate::quic::Config::default(), Parts::Streams)
	}

	/// Returns the configured versions, defaulting to all if none specified.
	pub fn versions(&self) -> moq_net::Versions {
		if self.version.is_empty() {
			moq_net::Versions::all()
		} else {
			moq_net::Versions::from(self.version.clone())
		}
	}

	/// Whether a QUIC, TCP, or Unix bind is explicitly configured.
	pub fn has_explicit_bind(&self) -> bool {
		self.bind.is_some() || self.has_stream_listener()
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

/// Default bind address used when [`crate::listen::Config::bind`] is not set.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
pub(crate) const DEFAULT_BIND: &str = "[::]:443";

/// Which listeners a [`Server`] opens, out of the ones its config describes.
///
/// Not configuration, which is why it is a constructor argument rather than a
/// field on [`crate::listen::Config`]: it says how *one process* splits its
/// listeners across threads, not what the process listens on. Keeping it out of
/// the config also keeps it off the clap and serde surface, where it would be a
/// flag nobody can set and a field every round-trip drops.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum Parts {
	/// Every listener the config asks for.
	#[default]
	All,

	/// The stream (`tcp`/`unix`) listeners only.
	Streams,

	/// The QUIC listener only, as one member of a `SO_REUSEPORT` group. Folding
	/// the slot in here is what stops a shard and a stream-only server from being
	/// asked for at once.
	Shard(crate::listen::Shard),
}

impl Parts {
	/// Whether a QUIC backend should be built.
	fn quic(self) -> bool {
		matches!(self, Self::All | Self::Shard(_))
	}

	/// Whether the `tcp`/`unix` listeners should be opened.
	#[cfg_attr(
		not(any(feature = "tcp", all(feature = "uds", unix))),
		expect(dead_code, reason = "no stream listener is compiled in")
	)]
	fn streams(self) -> bool {
		matches!(self, Self::All | Self::Streams)
	}

	/// This server's slot in the group, when it is a member of one.
	#[cfg_attr(
		not(any(feature = "noq", feature = "quinn", feature = "quiche")),
		expect(dead_code, reason = "no QUIC backend is compiled in")
	)]
	fn shard(self) -> Option<crate::listen::Shard> {
		match self {
			Self::Shard(shard) => Some(shard),
			_ => None,
		}
	}
}

/// Server for accepting MoQ connections.
///
/// Accepts QUIC (and optionally WebSocket), plus plaintext qmux over TCP
/// (`--listen-tcp-bind`) and Unix sockets (`--listen-unix-bind`). Create via
/// [`crate::listen::Config::init`] or [`Server::new`].
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
	/// Build a server from its config, binding the QUIC socket up front.
	///
	/// The stream (`tcp`/`unix`) listeners need a runtime, so they wait for
	/// [`listen`](Self::listen).
	pub fn new(config: crate::listen::Config, quic: crate::quic::Config) -> crate::Result<Self> {
		Self::build(config, quic, Parts::All)
	}

	/// [`Self::new`], for a caller that opens only some of the config's listeners.
	pub(crate) fn build(config: crate::listen::Config, quic: crate::quic::Config, parts: Parts) -> crate::Result<Self> {
		// Refuse here rather than in `init`, so a caller that skipped its own check
		// can't reach a listener that quietly ignored half of what it was given.
		let mut deprecated = config.deprecated();
		deprecated.extend(quic.deprecated());
		if !deprecated.is_empty() {
			return Err(Error::Deprecated(deprecated));
		}

		// Resolve here rather than in `init`, so a caller that builds the config by hand
		// gets the released spellings folded in too.
		config.validate()?;

		// `default_quic_backend` panics when no backend is compiled, so a WebSocket- or
		// stream-only build must not ask it.
		#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
		let backend = config.backend.clone().unwrap_or_else(crate::default_quic_backend);

		let versions = config.versions();

		// Build a QUIC backend when `--listen` is set, or when nothing else
		// is (the default). A stream-only server (`--listen-unix-bind` with no
		// `--listen`) doesn't also open UDP/443.
		quic.validate()?;

		let build_quic = parts.quic() && (config.bind.is_some() || !config.has_stream_listener());
		// `parts.quic()` and not `build_quic`: a caller that asked for streams only
		// is not asking for a backend, while leaving everything else configured is
		// still the error it always was.
		#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche")))]
		if config.bind.is_some() && parts.quic() {
			return Err(Error::NoBackend(
				"--listen requires a noq, quinn, or quiche backend feature",
			));
		}

		if build_quic && !config.tls.root.is_empty() {
			// Only a QUIC backend validates client certificates; the qmux listeners
			// (tcp/unix/websocket) carry no TLS of their own.
			#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
			let mtls_supported = match backend {
				#[cfg(feature = "quinn")]
				QuicBackend::Quinn => true,
				#[cfg(feature = "noq")]
				QuicBackend::Noq => true,
				#[cfg(feature = "quiche")]
				QuicBackend::Quiche => true,
				#[allow(unreachable_patterns)]
				_ => false,
			};
			#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche")))]
			let mtls_supported = false;

			if !mtls_supported {
				return Err(Error::MtlsUnsupported);
			}
		}

		#[cfg(feature = "noq")]
		#[allow(unreachable_patterns)]
		let noq = match backend {
			QuicBackend::Noq if build_quic => Some(crate::noq::NoqServer::new(config.clone(), &quic, parts.shard())?),
			_ => None,
		};

		#[cfg(feature = "quinn")]
		#[allow(unreachable_patterns)]
		let quinn = match backend {
			QuicBackend::Quinn if build_quic => {
				Some(crate::quinn::QuinnServer::new(config.clone(), &quic, parts.shard())?)
			}
			_ => None,
		};

		#[cfg(feature = "quiche")]
		let quiche = match backend {
			QuicBackend::Quiche if build_quic => {
				Some(crate::quiche::QuicheServer::new(config.clone(), &quic, parts.shard())?)
			}
			_ => None,
		};

		// Collect the configured stream listeners (at most one TCP, one Unix).
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		let mut stream_binds = Vec::new();
		#[cfg(feature = "tcp")]
		if let Some(addr) = config.tcp.bind.filter(|_| parts.streams()) {
			stream_binds.push(StreamBind::Tcp(addr));
		}
		#[cfg(all(feature = "uds", unix))]
		if let Some(path) = config.unix.bind.clone().filter(|_| parts.streams()) {
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
	pub fn with_websocket(mut self, websocket: crate::websocket::Listener) -> Self {
		self.websocket = Some(websocket);
		self
	}

	/// Also accept sessions over the given Iroh endpoint.
	#[cfg(feature = "iroh")]
	pub fn with_iroh(mut self, iroh: iroh::Endpoint) -> Self {
		self.iroh = Some(iroh);
		self
	}

	/// Publish the given origin to every session this server accepts.
	pub fn with_publisher(mut self, publish: impl moq_net::Consume<moq_net::origin::Consumer>) -> Self {
		self.moq = self.moq.with_publisher(publish);
		self
	}

	/// Subscribe to every session's broadcasts, ingesting them into the given origin.
	pub fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		self.moq = self.moq.with_subscriber(subscribe);
		self
	}

	/// Attach a per-connection [`moq_net::stats::Session`] context to all sessions
	/// accepted by this server.
	pub fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.moq = self.moq.with_stats(stats);
		self
	}

	/// Accept sessions until the listener stops, serving `origin` to each subscriber.
	///
	/// Spawns a task per session and logs (rather than propagates) per-session
	/// errors, so one bad peer never tears down the listener. Returns on a fatal
	/// bind failure, or once every listener has stopped; no process signal ends it,
	/// so race it against your own ctrl-C future to stop on one. For per-session
	/// auth or routing, [`listen`](Self::listen) and drive [`Listener::accept`]
	/// yourself instead.
	pub async fn serve_publish(self, origin: moq_net::origin::Consumer) -> crate::Result<()> {
		self.with_publisher(origin).serve().await
	}

	/// Accept sessions until the listener stops, ingesting each publisher into `origin`.
	///
	/// The mirror of [`serve_publish`](Self::serve_publish) for the consume direction.
	pub async fn serve_consume(self, origin: moq_net::origin::Producer) -> crate::Result<()> {
		self.with_subscriber(origin).serve().await
	}

	/// Accept sessions until the listener stops, serving `publish` to each subscriber
	/// and ingesting each publisher into `subscribe`.
	///
	/// The both-directions counterpart of [`serve_publish`](Self::serve_publish) and
	/// [`serve_consume`](Self::serve_consume), so an inbound session can subscribe to
	/// the origin and publish into it over one connection.
	pub async fn serve_both(
		self,
		publish: moq_net::origin::Consumer,
		subscribe: moq_net::origin::Producer,
	) -> crate::Result<()> {
		self.with_publisher(publish).with_subscriber(subscribe).serve().await
	}

	/// Shared accept loop for the `serve_*` entry points; the origin is already
	/// attached. Private so a server can't be served with no direction at all, which
	/// would accept and handshake sessions that carry nothing.
	async fn serve(self) -> crate::Result<()> {
		let mut listener = self.listen().await?;
		if let Ok(addr) = listener.local_addr() {
			tracing::info!(%addr, "listening");
		}
		while let Some(request) = listener.accept().await {
			tokio::spawn(async move {
				if let Err(err) = serve_session(request).await {
					tracing::warn!(%err, "session ended with error");
				}
			});
		}
		Ok(())
	}

	/// A live handle to the certificates this server is serving.
	///
	/// Use it to publish the SHA-256 fingerprints of a generated certificate at
	/// `/certificate.sha256`, which an `http://` client pins to reach a
	/// self-signed server. The handle tracks cert hot reloads, so hold it rather
	/// than the values it returns.
	///
	/// Empty when no TLS-bearing backend is configured (e.g. a stream-only server).
	pub fn certificates(&self) -> crate::tls::Certificates {
		#[cfg(feature = "noq")]
		if let Some(noq) = self.noq.as_ref() {
			return noq.certificates();
		}
		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_ref() {
			return quinn.certificates();
		}
		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_ref() {
			return quiche.certificates();
		}
		// No QUIC backend (e.g. a stream-only `--listen-tcp-bind`): no certificates.
		crate::tls::Certificates::empty()
	}

	#[cfg(not(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		all(feature = "uds", unix)
	)))]
	/// Returns the next partially established session.
	///
	/// Panics: no transport feature is compiled in, so nothing can be accepted.
	async fn accept_next(&mut self) -> Option<Request> {
		unreachable!("no transport compiled; enable a QUIC backend, websocket, tcp, or uds feature");
	}

	/// The accept-loop health of every listener this server owns that performs a real
	/// `accept(2)`: the `tcp`/`unix` stream listeners and, if one was set,
	/// [`with_websocket`](Self::with_websocket).
	///
	/// Empty on a QUIC-only server, which is the honest answer rather than a
	/// convenient one: a QUIC backend multiplexes every session over one UDP socket,
	/// so it never calls `accept` and has nothing that could fail this way. Publishing
	/// a zero for it would read as a watch that is passing when it can never fire.
	///
	/// Available before [`listen`](Self::listen), so an owner can register these with
	/// a metrics endpoint at startup even though the sockets bind there.
	pub fn accept_health(&self) -> Vec<crate::accept::Health> {
		#[allow(unused_mut)]
		let mut health = Vec::new();
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		health.extend(self.streams.health.iter().cloned());
		#[cfg(feature = "websocket")]
		health.extend(self.websocket.as_ref().map(|ws| ws.accept_health()));
		health
	}

	/// Start serving: bind whatever is still unbound and hand back the
	/// [`Listener`] to accept sessions from.
	///
	/// Terminal, and that is the point: it consumes the `Server`, so the builders
	/// above cannot run afterwards and every session is served the configuration
	/// this call captured. The QUIC socket is bound by [`crate::listen::Config::init`], but
	/// the stream (`tcp`/`unix`) listeners need a runtime, so they bind here.
	///
	/// A bind failure is the error, not a silent `None` from a later accept. It
	/// leaves nothing bound: the partially built `Listener` drops here, closing
	/// whatever it opened. Build a fresh `Server` from the (cloneable) config to try
	/// again.
	// `mut` is only needed to bind the stream listeners, which a QUIC-only build has none of.
	#[allow(unused_mut)]
	pub async fn listen(mut self) -> crate::Result<Listener> {
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		{
			// The stream listeners offer a wider version set than the server's own
			// (see `stream_versions`), against the same configuration.
			let server = self.moq.clone().with_versions(self.streams.versions.clone());
			self.streams.start(&server).await?;
		}
		Ok(Listener { server: self })
	}

	/// The body of [`Listener::accept`]; the listeners are already running.
	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		all(feature = "uds", unix)
	))]
	async fn accept_next(&mut self) -> Option<Request> {
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
					Some(ws) => ws.accept_with_url().await,
					None => None,
				}
			};
			#[cfg(not(feature = "websocket"))]
			let ws_accept = std::future::ready(None::<crate::Result<()>>);

			#[allow(unused_variables)]
			let server = self.moq.clone();
			#[allow(unused_variables)]
			let versions = self.versions.clone();

			// An absent transport resolves `None` rather than parking, so its arm is
			// disabled instead of holding the `select!` open. That is what lets `else`
			// mean "nothing is left that could accept" instead of "the one transport
			// nobody configured is still notionally pending".
			#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
			let stream_accept = self.streams.recv();
			#[cfg(not(any(feature = "tcp", all(feature = "uds", unix))))]
			let stream_accept = std::future::ready(None::<Request>);

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
							let request = server.accept_request(crate::transport::Async::new(session)).await?;
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
						let request = server.accept_request(crate::transport::Async::new(session)).await?;
						Ok(Request { transport: Transport::Iroh, url, identity, kind: RequestKind::Iroh(Box::new(request)) })
					}.boxed());
				}
				Some(_res) = ws_accept => {
					#[cfg(feature = "websocket")]
					match _res {
						Ok((session, url)) => {
							// Read the SETUP off the qmux session before handing it over, so a
							// slow peer doesn't stall the accept loop (spawned like the others).
							self.accept.push(async move {
								let request = server.accept_request(crate::transport::Async::new(session)).await?;
								Ok(Request { transport: Transport::WebSocket, url: Some(url), identity: None, kind: RequestKind::Qmux(Box::new(request)) })
							}.boxed());
						}
						// One connection's upgrade, not the listener's: a failed
						// `accept(2)` never reaches here, having been classified,
						// counted, and warned about by the listener itself.
						Err(err) => tracing::debug!(%err, "WebSocket upgrade failed"),
					}
				}
				Some(res) = self.accept.next() => {
					match res {
						Ok(session) => return Some(session),
						Err(err) => tracing::debug!(%err, "failed to accept session"),
					}
				}
				// Nothing is left that could accept: every configured listener has stopped
				// and no handshake is still in flight. Process signals are the owner's; an
				// accept loop that consumes a global ctrl-C leaves no way to sequence
				// shutdown around it (moq-relay drains sessions first).
				else => return None,
			}
		}
	}

	/// The Iroh endpoint from [`with_iroh`](Self::with_iroh), if one was set.
	#[cfg(feature = "iroh")]
	pub fn iroh_endpoint(&self) -> Option<&iroh::Endpoint> {
		self.iroh.as_ref()
	}

	/// The address the QUIC listener bound to, useful when the config asked for
	/// port 0.
	///
	/// Errors with [`Error::NoBackend`] on a stream-only server, which has no
	/// QUIC listener.
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
		// No QUIC backend (e.g. a stream-only `--listen-tcp-bind`).
		Err(Error::NoBackend("no QUIC listener configured"))
	}

	/// The address the WebSocket listener from
	/// [`with_websocket`](Self::with_websocket) bound to, if one was set.
	#[cfg(feature = "websocket")]
	pub fn websocket_local_addr(&self) -> Option<net::SocketAddr> {
		self.websocket.as_ref().and_then(|ws| ws.local_addr().ok())
	}

	/// The body of [`Listener::close`].
	async fn shutdown(&mut self) {
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		self.streams.shutdown().await;

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
	}
}

/// A [`Server`] that is listening: the only thing sessions can be accepted from.
///
/// Returned by [`Server::listen`], which consumes the `Server`, so the
/// configuration a session is served with is fixed before any listener runs.
pub struct Listener {
	server: Server,
}

impl Listener {
	/// Returns the next partially established session, across every configured
	/// transport (QUIC, WebSocket, and plaintext qmux over TCP/Unix).
	///
	/// This returns a [Request] instead of a session so the connection can be
	/// rejected early on an invalid path or missing auth. Call [Request::ok] or
	/// [Request::close] to complete the handshake.
	///
	/// `None` means every configured listener has stopped and no handshake is still
	/// in flight, so nothing can arrive again. Everything is already bound, so a bind
	/// failure cannot arrive here. No process signal is watched: race this against
	/// your own ctrl-C future and drop the listener if you want one to stop the loop.
	pub async fn accept(&mut self) -> Option<Request> {
		self.server.accept_next().await
	}

	/// Close every listener, giving in-flight connections a moment to see the
	/// shutdown.
	///
	/// Consumes the listener so its bound sockets are released before this returns.
	pub async fn close(mut self) {
		self.server.shutdown().await;
	}

	/// The address the QUIC listener bound to, useful when the config asked for
	/// port 0.
	///
	/// Errors with [`Error::NoBackend`] on a stream-only server, which has no
	/// QUIC listener.
	pub fn local_addr(&self) -> crate::Result<net::SocketAddr> {
		self.server.local_addr()
	}

	/// The address the WebSocket listener from
	/// [`Server::with_websocket`] bound to, if one was set.
	#[cfg(feature = "websocket")]
	pub fn websocket_local_addr(&self) -> Option<net::SocketAddr> {
		self.server.websocket_local_addr()
	}

	/// A live handle to the certificates this server is serving.
	///
	/// See [`Server::certificates`], which is also readable before listening.
	pub fn certificates(&self) -> crate::tls::Certificates {
		self.server.certificates()
	}

	/// The accept-loop health of every listener that performs a real `accept(2)`.
	///
	/// See [`Server::accept_health`], which is also readable before listening.
	pub fn accept_health(&self) -> Vec<crate::accept::Health> {
		self.server.accept_health()
	}

	/// The Iroh endpoint from [`Server::with_iroh`], if one was set.
	#[cfg(feature = "iroh")]
	pub fn iroh_endpoint(&self) -> Option<&iroh::Endpoint> {
		self.server.iroh_endpoint()
	}
}

/// Complete one accepted [`Request`] and wait for the session to close.
async fn serve_session(request: Request) -> crate::Result<()> {
	let session = request.ok().await?;
	Err(session.closed().await.into())
}

/// The version set offered on stream (`tcp://`/`unix://`) listeners.
///
/// A URL-less transport carries the request path in the moq-lite-05 SETUP, so
/// lite-05 is offered on top of the configured versions even when a custom set
/// omits it. Older versions still work for clients that need no path.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
fn stream_versions(base: &moq_net::Versions) -> moq_net::Versions {
	let mut versions: Vec<moq_net::Version> = base.iter().copied().collect();
	if let Ok(lite05) = "moq-lite-05".parse::<moq_net::Version>()
		&& !versions.contains(&lite05)
	{
		versions.push(lite05);
	}
	moq_net::Versions::from(versions)
}

/// A configured stream listener (`--listen-tcp-bind` / `--listen-unix-bind`).
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
enum StreamBind {
	#[cfg(feature = "tcp")]
	Tcp(net::SocketAddr),
	#[cfg(all(feature = "uds", unix))]
	Unix(PathBuf),
}

/// A bound stream listener, before its accept loop is spawned.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
enum BoundListener {
	#[cfg(feature = "tcp")]
	Tcp(crate::tcp::Listener),
	#[cfg(all(feature = "uds", unix))]
	Unix(crate::unix::Listener),
}

#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
impl StreamBind {
	/// The name this listener reports its accept health under.
	fn name(&self) -> &'static str {
		match self {
			#[cfg(feature = "tcp")]
			Self::Tcp(_) => "tcp",
			#[cfg(all(feature = "uds", unix))]
			Self::Unix(_) => "unix",
		}
	}
}

/// The stream (`tcp`/`unix`) listeners owned by a [`Server`].
///
/// Bound by [`Server::listen`] (they need a runtime), after which each runs an
/// accept loop in its own task and feeds completed [`Request`]s back over a channel.
/// The tasks own their listeners and are stopped when the [`Listener`] closes or
/// drops, so bound sockets don't linger.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
struct StreamListeners {
	binds: Vec<StreamBind>,
	/// One per entry in `binds`, in the same order, and created up front rather than
	/// with the listener: an owner registering these with a metrics endpoint does so
	/// at startup, long before the first `accept` binds anything.
	health: Vec<crate::accept::Health>,
	versions: moq_net::Versions,
	#[cfg(all(feature = "uds", unix))]
	unix_allow: Option<crate::unix::Allow>,
	rx: Option<tokio::sync::mpsc::Receiver<Request>>,
	tasks: Vec<tokio::task::JoinHandle<()>>,
}

#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
impl StreamListeners {
	fn new(
		binds: Vec<StreamBind>,
		versions: moq_net::Versions,
		#[cfg(all(feature = "uds", unix))] unix_allow: Option<crate::unix::Allow>,
	) -> Self {
		let health = binds
			.iter()
			.map(|bind| crate::accept::Health::new(bind.name()))
			.collect();
		Self {
			binds,
			health,
			versions,
			#[cfg(all(feature = "uds", unix))]
			unix_allow,
			rx: None,
			tasks: Vec::new(),
		}
	}

	/// Bind every configured listener and spawn its accept loop.
	///
	/// Called once, from [`Server::listen`]. Everything binds before anything is
	/// spawned, so a failure part-way drops the listeners already opened and frees
	/// their sockets there and then, rather than leaving accept loops to be aborted
	/// at some later point.
	///
	/// `server` is the configuration each accepted session handshakes against,
	/// already fixed by the time this runs.
	async fn start(&mut self, server: &moq_net::Server) -> crate::Result<()> {
		if self.binds.is_empty() {
			return Ok(());
		}

		let mut bound = Vec::with_capacity(self.binds.len());
		for (bind, health) in self.binds.drain(..).zip(self.health.iter().cloned()) {
			let alpns = self.versions.alpns();
			match bind {
				#[cfg(feature = "tcp")]
				StreamBind::Tcp(addr) => {
					if !addr.ip().is_loopback() {
						tracing::warn!(%addr, "tcp listener bound to a non-loopback address; qmux is UNENCRYPTED, ensure the network is trusted");
					}
					let listener = crate::tcp::Listener::bind(addr)
						.await?
						.with_protocols(alpns)
						.with_accept_health(health);
					tracing::info!(%addr, "listening (tcp)");
					bound.push(BoundListener::Tcp(listener));
				}
				#[cfg(all(feature = "uds", unix))]
				StreamBind::Unix(path) => {
					let listener = crate::unix::Listener::bind(&path)
						.await?
						.with_protocols(alpns)
						.with_accept_health(health);
					// Loose socket perms let workers run as a different user. The parent
					// directory or uid/gid/pid allowlist is the access gate.
					listener.set_mode(0o666)?;
					tracing::info!(path = %path.display(), allow = ?self.unix_allow, "listening (unix)");
					bound.push(BoundListener::Unix(listener));
				}
			}
		}

		let (tx, rx) = tokio::sync::mpsc::channel(16);
		for listener in bound {
			let task = match listener {
				#[cfg(feature = "tcp")]
				BoundListener::Tcp(listener) => spawn_tcp_loop(listener, server.clone(), tx.clone()),
				#[cfg(all(feature = "uds", unix))]
				BoundListener::Unix(listener) => spawn_unix_loop(listener, server.clone(), self.unix_allow.clone(), tx.clone()),
			};
			self.tasks.push(task);
		}

		self.rx = Some(rx);
		Ok(())
	}

	/// Yield the next stream [`Request`], or `None` if no listener is running:
	/// either none was configured, or every accept loop has ended.
	async fn recv(&mut self) -> Option<Request> {
		match self.rx.as_mut() {
			Some(rx) => rx.recv().await,
			None => None,
		}
	}

	/// Stop every accept loop and wait until its listener has released the socket.
	async fn shutdown(&mut self) {
		self.binds.clear();
		self.rx = None;
		for task in self.tasks.drain(..) {
			task.abort();
			let _ = task.await;
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
	server: moq_net::Server,
	tx: tokio::sync::mpsc::Sender<Request>,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		loop {
			match listener.accept().await {
				Some(Ok(session)) => spawn_stream_request(session, Transport::Tcp, server.clone(), tx.clone()),
				// Per-connection: a failed `accept(2)` is the listener's own to
				// classify and pace, and never surfaces here.
				Some(Err(err)) => tracing::warn!(%err, "tcp qmux handshake failed"),
				None => break,
			}
		}
	})
}

#[cfg(all(feature = "uds", unix))]
fn spawn_unix_loop(
	listener: crate::unix::Listener,
	server: moq_net::Server,
	allow: Option<crate::unix::Allow>,
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
					spawn_stream_request(session, Transport::Unix, server.clone(), tx.clone());
				}
				// Per-connection, as in `spawn_tcp_loop`.
				Some(Err(err)) => tracing::warn!(%err, "unix qmux handshake failed"),
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
	server: moq_net::Server,
	tx: tokio::sync::mpsc::Sender<Request>,
) {
	tokio::spawn(async move {
		match server.accept_request(crate::transport::Async::new(session)).await {
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
	Noq(Box<moq_net::Request<crate::transport::Async<web_transport_noq::Session>>>),
	#[cfg(feature = "quinn")]
	Quinn(Box<moq_net::Request<web_transport_quinn::Session>>),
	#[cfg(feature = "quiche")]
	Quiche(Box<moq_net::Request<web_transport_quiche::Connection>>),
	#[cfg(feature = "iroh")]
	Iroh(Box<moq_net::Request<crate::transport::Async<web_transport_iroh::Session>>>),
	#[cfg(any(feature = "tcp", all(feature = "uds", unix), feature = "websocket"))]
	Qmux(Box<moq_net::Request<crate::transport::Async<qmux::Session>>>),
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
	/// The request URL, for transports that carry one (QUIC/WebTransport/WebSocket). `None` for the
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

	/// Attach a per-connection [`moq_net::stats::Session`] context to this session.
	pub fn with_stats(self, stats: moq_net::stats::Session) -> Self {
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
		let pair = request_into!(self.kind, request => request.ok().await?);
		Ok(crate::spawn_session(pair))
	}

	/// Returns the network transport carrying this session.
	pub fn transport(&self) -> Transport {
		self.transport
	}

	/// Returns the request URL for transports that carry one (QUIC/WebTransport/WebSocket).
	///
	/// `None` for the URL-less stream bindings (`tcp`/`unix`); use [`Self::path`] for their
	/// in-band request path.
	pub fn url(&self) -> Option<&Url> {
		self.url.as_ref()
	}

	/// The request path the client advertised, uniform across transports.
	///
	/// Taken from the SETUP for the URL-less stream bindings (and moq-transport, which
	/// carries it in-band), or the request [`url`](Self::url) for
	/// WebTransport/QUIC/WebSocket.
	/// The missing or root path is returned as an empty string.
	pub fn path(&self) -> &str {
		// An empty SETUP path means the client advertised none, so fall back to the
		// request URL. URL-carrying bindings are the ones that must not send a path at
		// all, so this never discards a path the client meant us to use.
		let setup = request_ref!(self, r => r.path());
		let path = if setup.is_empty() {
			self.url.as_ref().map(Url::path).unwrap_or("")
		} else {
			setup.split_once('?').map_or(setup, |(path, _)| path)
		};
		if path == "/" { "" } else { path }
	}

	/// The encoded request query without the leading `?`, if one was advertised.
	///
	/// Query values can contain credentials. Avoid logging this value.
	pub fn query(&self) -> Option<&str> {
		let setup = request_ref!(self, r => r.path());
		if setup.is_empty() {
			self.url.as_ref().and_then(Url::query)
		} else {
			setup.split_once('?').map(|(_, query)| query)
		}
	}

	/// The single direction the client advertised in its SETUP, or `None` for a
	/// bidirectional session (it omitted the role, or the version carries none).
	/// Available on every transport. Use it to reject a token that lacks the scope for
	/// the client's intended direction.
	pub fn role(&self) -> Option<moq_net::Role> {
		request_ref!(self, r => r.role())
	}

	/// The origin identity the peer declared in its SETUP (moq-lite-05+).
	///
	/// A peer declares this when it attaches a publish or subscribe origin.
	/// Older versions and peers without one return `None`.
	///
	/// Self-declared, so treat it as a correlation hint rather than an
	/// authenticated identity: authorize on the token or client certificate.
	pub fn peer_origin(&self) -> Option<moq_net::Origin> {
		request_ref!(self, r => r.peer_origin())
	}

	/// The client certificate chain the peer presented, if any, validated
	/// against a configured [`crate::tls::Listen::root`] during the handshake.
	///
	/// Captured at the transport handshake (before the SETUP). Only the Quinn and noq
	/// backends support mTLS; other transports always return `None`. Use it to grant
	/// elevated access or to close the session once the certificate expires (see
	/// [`crate::tls::PeerIdentity::expiry`]).
	pub fn peer_identity(&self) -> Option<crate::tls::PeerIdentity> {
		self.identity.clone()
	}

	#[doc(hidden)]
	#[deprecated(note = "use `peer_identity` instead")]
	pub fn has_peer_certificate(&self) -> bool {
		self.peer_identity().is_some()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn version_help_lists_every_parseable_name() {
		let help = <crate::listen::Config as clap::Args>::augment_args(clap::Command::new("test"))
			.render_long_help()
			.to_string();
		for name in moq_net::Version::names() {
			assert!(help.contains(name), "missing {name} from --server-version help");
		}
	}

	/// The handles have to exist before anything binds, and cover the stream
	/// listeners rather than just the ones an owner happens to construct itself.
	///
	/// `tcp`/`unix` bind in `listen`, so a naive implementation hands out nothing at
	/// startup, which is exactly when a metrics endpoint is assembled. A stream-only
	/// node would then publish no accept counters for the only sockets on it that can
	/// fail.
	#[cfg(feature = "tcp")]
	#[test]
	fn accept_health_covers_stream_listeners_before_they_bind() {
		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let server = Server::new(config, Default::default()).expect("stream-only server");

		let names: Vec<_> = server.accept_health().iter().map(|h| h.listener()).collect();
		assert_eq!(names, vec!["tcp"], "the tcp listener must report before it binds");
	}

	/// A failed `listen` must leave nothing bound.
	///
	/// The tcp listener binds before the unix one fails, so a partial bind is the
	/// case to get right: its socket has to be released on the way out, or the port
	/// stays held by a server that never serves. Binding everything before spawning
	/// anything is what makes that release immediate rather than whenever an aborted
	/// task happens to be dropped.
	#[cfg(all(feature = "tcp", feature = "uds", unix))]
	#[tokio::test]
	async fn a_failed_listen_binds_nothing() {
		// A path that cannot be a socket, so the unix bind fails after the tcp one
		// has already succeeded.
		let dir = tempfile::TempDir::new().unwrap();
		let occupied = dir.path().join("not-a-socket");
		std::fs::write(&occupied, b"in the way").unwrap();

		// A concrete port, so the release is observable.
		let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe");
		let port = probe.local_addr().expect("probe addr").port();
		drop(probe);

		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
		config.unix.bind = Some(occupied);
		let server = Server::new(config, Default::default()).expect("stream-only server");

		assert!(server.listen().await.is_err(), "the unix bind must fail");
		std::net::TcpListener::bind(("127.0.0.1", port)).expect("the tcp port must be free again");
	}

	/// Closing consumes the listener and immediately releases its stream sockets.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn close_releases_stream_listeners() {
		let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe");
		let addr = probe.local_addr().expect("probe addr");
		drop(probe);

		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some(addr);
		let listener = Server::new(config, Default::default())
			.expect("stream-only server")
			.listen()
			.await
			.expect("listen");

		listener.close().await;
		std::net::TcpListener::bind(addr).expect("close must release the tcp port");
	}

	/// The stream listeners must hand accepted sessions to the *configured*
	/// [`moq_net::Server`]. [`Server::serve_publish`] sets the publisher there
	/// rather than on the request, so a session that handshakes against any other
	/// server accepts and then serves nothing.
	#[cfg(all(feature = "uds", unix))]
	#[tokio::test]
	async fn unix_listener_serves_the_configured_publisher() {
		use rand::RngExt;

		// macOS caps AF_UNIX paths near 104 bytes and the system temp dir is long,
		// so bind under /tmp with a name unique to this process.
		let path = PathBuf::from(format!("/tmp/moq-tokio-publish-{}.sock", std::process::id()));
		let _ = std::fs::remove_file(&path);

		let origin = crate::origin::spawn(moq_net::Origin::random());
		let mut broadcast = origin
			.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
			.expect("create broadcast");
		let mut track = broadcast.create_track("video", None).expect("create track");
		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
			.expect("write frame");
		group.finish().expect("finish group");

		let mut config = crate::listen::Config::default();
		config.unix.bind = Some(path.clone());
		let server = config.init(Default::default()).expect("server init");

		// The publisher lives on the server, never on the accepted request.
		let serve = tokio::spawn(server.serve_publish(origin.consume()));

		// `serve` binds the socket; wait for it. Keep the last error, since a bind
		// failure is the only clue to why it never showed up.
		const MAX_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
		let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
		let mut delay = std::time::Duration::from_millis(1);
		while let Err(err) = tokio::net::UnixStream::connect(&path).await {
			assert!(
				tokio::time::Instant::now() < deadline,
				"unix listener never bound: {err}"
			);
			// Bound to its own statement so the (non-Send) rng doesn't live across the
			// await, which would make this future unspawnable.
			let wait = delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
			tokio::time::sleep(wait).await;
			delay = (delay * 2).min(MAX_DELAY);
		}

		const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

		let url: Url = format!("unix://{}", path.display()).parse().expect("parse url");
		let subscriber = crate::origin::spawn(moq_net::Origin::random());
		let mut announced = subscriber.consume().announced();
		let client = crate::connect::Config::default()
			.init(Default::default())
			.expect("client init")
			.with_subscriber(subscriber);
		let session = tokio::time::timeout(TIMEOUT, client.connect(url).established())
			.await
			.expect("connect timeout")
			.expect("connect");

		// Without the server's publisher the session announces nothing, so this is
		// where the regression shows up.
		let update = tokio::time::timeout(TIMEOUT, announced.next())
			.await
			.expect("announce timeout")
			.expect("origin closed");
		assert_eq!(update.path.as_str(), "test");
		let broadcast = update.broadcast.expect("expected an announce");

		let mut track = broadcast
			.track("video")
			.expect("track name")
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = tokio::time::timeout(TIMEOUT, track.recv_group())
			.await
			.expect("recv group timeout")
			.expect("recv group")
			.expect("track closed early");
		let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("read frame timeout")
			.expect("read frame")
			.expect("group closed early");
		assert_eq!(&frame.payload[..], b"hello");

		drop(session);
		serve.abort();
		let _ = std::fs::remove_file(&path);
	}

	/// Closing a listener must release its TCP socket before returning. Reusing it
	/// afterwards is unrepresentable: `listen` consumes the `Server` and `close`
	/// consumes the `Listener`.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn close_releases_stream_listener_socket() {
		let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = probe.local_addr().unwrap();
		drop(probe);

		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some(addr);
		let server = Server::new(config, Default::default()).expect("stream-only server");
		let listener = server.listen().await.expect("listen");
		assert!(tokio::net::TcpListener::bind(addr).await.is_err(), "listener is bound");

		listener.close().await;
		let _rebound = tokio::net::TcpListener::bind(addr)
			.await
			.expect("close must release the listener socket");
	}

	/// An explicit QUIC bind cannot be honored without a QUIC backend.
	#[cfg(not(any(feature = "noq", feature = "quinn", feature = "quiche")))]
	#[test]
	fn quic_bind_without_a_quic_backend_is_rejected() {
		let config = crate::listen::Config {
			bind: Some("127.0.0.1:0".to_string()),
			..Default::default()
		};

		assert!(matches!(
			Server::new(config, Default::default()),
			Err(Error::NoBackend(_))
		));
	}

	/// A QUIC-only server reports nothing. It multiplexes over one UDP socket and
	/// never calls `accept`, so a zero counter would be a watch that cannot fire.
	#[cfg(all(feature = "quinn", not(feature = "tcp")))]
	#[test]
	fn accept_health_is_empty_without_a_stream_listener() {
		let server = crate::listen::Config::default()
			.init(Default::default())
			.expect("quic server");
		assert!(server.accept_health().is_empty());
	}

	#[test]
	fn transport_names_are_stable() {
		assert_eq!(Transport::Quic.as_str(), "quic");
		assert_eq!(Transport::Iroh.as_str(), "iroh");
		assert_eq!(Transport::WebSocket.as_str(), "websocket");
		assert_eq!(Transport::Tcp.as_str(), "tcp");
		assert_eq!(Transport::Unix.as_str(), "unix");
	}

	/// Building the endpoint needs a runtime, and `certificates()` must stay
	/// readable without one (no guard escapes to the caller).
	#[cfg(feature = "quinn")]
	#[tokio::test]
	async fn certificates_expose_generated_fingerprints() {
		let mut config = crate::listen::Config {
			bind: Some("[::]:0".to_string()),
			..Default::default()
		};
		config.tls.generate = vec!["localhost".into()];

		let certs = config.init(Default::default()).expect("server init").certificates();
		let fingerprints = certs.fingerprints();
		assert_eq!(fingerprints.len(), 1, "one generated certificate");
		// Hex-encoded SHA-256.
		assert_eq!(fingerprints[0].len(), 64);
		assert!(fingerprints[0].chars().all(|c| c.is_ascii_hexdigit()));
	}

	/// A stream-only server has no TLS backend, so there's nothing to pin. This
	/// must report empty rather than panic.
	#[cfg(all(feature = "uds", unix))]
	#[tokio::test]
	async fn certificates_are_empty_without_a_tls_backend() {
		let mut config = crate::listen::Config::default();
		config.unix.bind = Some(PathBuf::from("/tmp/moq-tokio-certificates-test.sock"));

		let server = config.init(Default::default()).expect("server init");
		assert!(server.certificates().fingerprints().is_empty());
	}

	#[test]
	fn test_tls_string_or_array() {
		// Single string should deserialize into a Vec with one entry.
		let single = r#"
			cert = "cert.pem"
			key = "key.pem"
		"#;
		let config: crate::tls::Listen = toml::from_str(single).unwrap();
		assert_eq!(config.cert, vec![PathBuf::from("cert.pem")]);
		assert_eq!(config.key, vec![PathBuf::from("key.pem")]);

		// TOML arrays should still work.
		let array = r#"
			cert = ["a.pem", "b.pem"]
			key = ["a.key", "b.key"]
			generate = ["localhost"]
			root = ["ca.pem"]
		"#;
		let config: crate::tls::Listen = toml::from_str(array).unwrap();
		assert_eq!(config.cert, vec![PathBuf::from("a.pem"), PathBuf::from("b.pem")]);
		assert_eq!(config.key, vec![PathBuf::from("a.key"), PathBuf::from("b.key")]);
		assert_eq!(config.generate, vec!["localhost".to_string()]);
		assert_eq!(config.root, vec![PathBuf::from("ca.pem")]);
	}

	#[test]
	fn bind_string_or_listen_alias() {
		// The QUIC bind is a plain address; the `listen` alias still works.
		let bind: crate::listen::Config = toml::from_str(r#"bind = "[::]:443""#).unwrap();
		assert_eq!(bind.bind.as_deref(), Some("[::]:443"));

		let alias: crate::listen::Config = toml::from_str(r#"listen = "0.0.0.0:4443""#).unwrap();
		assert_eq!(alias.bind.as_deref(), Some("0.0.0.0:4443"));
	}

	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn stream_listener_config_parses() {
		let config: crate::listen::Config = toml::from_str(
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
		assert!(config.has_explicit_bind());
	}

	#[cfg(all(feature = "uds", unix))]
	#[test]
	fn stream_only_config_has_no_quic() {
		// A unix listener with no `--listen` is stream-only.
		let mut config = crate::listen::Config::default();
		config.unix.bind = Some(PathBuf::from("/run/moq.sock"));
		assert!(config.has_stream_listener());
		assert!(config.has_explicit_bind());
		assert!(config.bind.is_none());

		// The default (nothing configured) still runs QUIC.
		assert!(!crate::listen::Config::default().has_stream_listener());
		assert!(!crate::listen::Config::default().has_explicit_bind());
	}
}
