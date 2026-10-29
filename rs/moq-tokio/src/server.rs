//! Accepting peers: the [`Server`], its [`Request`]s, and the [`Config`] it is built from.
//!
//! [`Config`] pairs the accept half of an endpoint ([`crate::listen::Config`]) with the
//! QUIC settings ([`crate::quic::Config`]) a binary shares with its dial half. The dial
//! side is [`crate::client`].

use std::net;
#[cfg(any(test, all(feature = "uds", unix)))]
use std::path::PathBuf;

#[cfg(feature = "iroh")]
use crate::iroh;
use crate::{Error, Transport};
use moq_net::Session;
use url::Url;

// Only the transports that finish their handshake in a spawned future need `.boxed()`;
// the stream listeners hand back an already-built `Request`.
#[cfg(any(feature = "quic", feature = "iroh", feature = "websocket"))]
use futures::FutureExt;
// The pending handshakes of the transports `Server` accepts itself; WebRTC
// signaling is the caller's, so a WebRTC-only build has none.
#[cfg(any(
	feature = "quic",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	all(feature = "uds", unix)
))]
use futures::{
	future::BoxFuture,
	stream::{FuturesUnordered, StreamExt},
};

impl crate::listen::Config {
	/// Build the [`Server`] this config describes, binding its listeners.
	pub fn init(self, quic: crate::quic::Config) -> crate::Result<Server> {
		Config {
			listen: self,
			quic,
			..Default::default()
		}
		.init()
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
		Config {
			listen: self,
			..Default::default()
		}
		.init_streams()
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
	#[allow(unused_mut, reason = "only the stream listener features set it")]
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
#[cfg(feature = "quic")]
pub(crate) const DEFAULT_BIND: net::SocketAddr =
	net::SocketAddr::V6(net::SocketAddrV6::new(net::Ipv6Addr::UNSPECIFIED, 443, 0, 0));

/// Which listeners a [`Server`] opens, out of the ones its config describes.
///
/// Not configuration, which is why it is a constructor argument rather than a
/// field on [`crate::listen::Config`]: it says how *one process* splits its
/// listeners across threads, not what the process listens on. Keeping it out of
/// the config also keeps it off the Usage and serde surface, where it would be a
/// flag nobody can set and a field every round-trip drops.
#[derive(Debug, Default)]
pub(crate) enum Parts {
	/// Every listener the config asks for.
	#[default]
	All,

	/// The stream (`tcp`/`unix`) listeners only.
	Streams,

	/// The QUIC listener only, as one member of a `SO_REUSEPORT` group. Folding
	/// the member in here is what stops a group member and a stream-only server
	/// from being asked for at once.
	#[cfg_attr(not(feature = "quic"), expect(dead_code, reason = "no QUIC backend is compiled in"))]
	Member(crate::listen::Socket),
}

impl Parts {
	/// Whether a QUIC backend should be built.
	fn quic(&self) -> bool {
		matches!(self, Self::All | Self::Member(_))
	}

	/// Whether the `tcp`/`unix` listeners should be opened.
	#[cfg_attr(
		not(any(feature = "tcp", all(feature = "uds", unix))),
		expect(dead_code, reason = "no stream listener is compiled in")
	)]
	fn streams(&self) -> bool {
		matches!(self, Self::All | Self::Streams)
	}

	/// This server's socket in a complete group, when it is a member of one.
	/// Consuming, because only one backend may own the serving handle.
	#[cfg_attr(not(feature = "quic"), expect(dead_code, reason = "no QUIC backend is compiled in"))]
	fn member(self) -> Option<crate::listen::Socket> {
		match self {
			Self::Member(member) => Some(member),
			_ => None,
		}
	}
}

/// Everything a [`Server`] is built from.
///
/// Distinct from [`crate::listen::Config`], which is only the accept half of an endpoint:
/// this pairs that half with the [`quic::Config`](crate::quic::Config) a binary shares
/// between listening and dialing, because a `Server` needs both and neither owns the
/// other. Grouping them here is what lets a future knob land as a field rather than as
/// another [`Server::new`] parameter. The mirror of [`crate::client::Config`].
///
/// Most callers want the [`crate::listen::Config::init`] shorthand instead.
#[derive(Default)]
#[non_exhaustive]
pub struct Config {
	/// Shared served identity; `listen.tls` must be empty when this is set.
	#[cfg(feature = "_certs")]
	pub tls: Option<crate::tls::Serve>,

	/// Tracks accepted transports through their handshake and session lifetime.
	pub activity: moq_net::server::Activity,
	/// The accept side of the endpoint: what to listen on and how to be trusted.
	pub listen: crate::listen::Config,

	/// QUIC socket and transport settings, shared with [`crate::Client`].
	pub quic: crate::quic::Config,

	/// A standalone WebSocket listener on a separate TCP port.
	#[cfg(feature = "websocket")]
	pub websocket: Option<crate::websocket::Listener>,

	/// An Iroh endpoint to accept sessions from.
	///
	/// The server owns the endpoint's listener: it replaces the endpoint's ALPN set with its
	/// configured versions, accepts every incoming connection, and closes it on shutdown.
	/// Don't give one endpoint to two servers; dialing through clones of it is fine.
	#[cfg(feature = "iroh")]
	pub iroh: Option<iroh::Endpoint>,

	/// The origin published to every accepted session.
	pub publisher: Option<moq_net::origin::Consumer>,

	/// The origin that receives every accepted session's publications.
	pub subscriber: Option<moq_net::origin::Producer>,

	/// Per-connection statistics context.
	pub stats: moq_net::stats::Session,
}

impl Config {
	/// Build the [`Server`] this config describes.
	pub fn init(self) -> crate::Result<Server> {
		Server::new(self)
	}

	/// Build a server with only its TCP and Unix listeners.
	pub fn init_streams(self) -> crate::Result<Server> {
		Server::build(self, Parts::Streams)
	}

	/// Copy the settings one QUIC worker owns.
	#[cfg(feature = "quic")]
	pub(crate) fn worker(&self) -> Self {
		Self {
			listen: self.listen.clone(),
			#[cfg(feature = "_certs")]
			tls: self.tls.clone(),
			quic: self.quic.clone(),
			#[cfg(feature = "websocket")]
			websocket: None,
			#[cfg(feature = "iroh")]
			iroh: None,
			publisher: self.publisher.clone(),
			subscriber: self.subscriber.clone(),
			stats: self.stats.clone(),
			activity: self.activity.clone(),
		}
	}
}

/// Server for accepting MoQ connections.
///
/// Accepts QUIC (and optionally WebSocket), plus plaintext qmux over TCP
/// (`--listen-tcp-bind`) and Unix sockets (`--listen-unix-bind`). Create via
/// [`crate::listen::Config::init`] or [`Server::new`].
pub struct Server {
	activity: moq_net::server::Activity,
	moq: moq_net::Server,
	versions: moq_net::Versions,
	/// From [`crate::listen::Config::resolved_timeout`].
	timeout: Option<std::time::Duration>,
	#[cfg(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		all(feature = "uds", unix)
	))]
	accept: FuturesUnordered<BoxFuture<'static, crate::Result<Request>>>,
	#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
	streams: StreamListeners,
	#[cfg(feature = "iroh")]
	iroh: Option<iroh::Endpoint>,
	#[cfg(feature = "quic")]
	quic: Option<crate::quic::Server>,
	#[cfg(feature = "websocket")]
	websocket: Option<crate::websocket::Listener>,
}

impl Server {
	/// Build a server from its config, binding the QUIC socket up front.
	///
	/// The stream (`tcp`/`unix`) listeners bind asynchronously, so they wait for
	/// [`listen`](Self::listen).
	pub fn new(config: Config) -> crate::Result<Self> {
		Self::build(config, Parts::All)
	}

	/// [`Self::new`], for a caller that opens only some of the config's listeners.
	pub(crate) fn build(config: Config, parts: Parts) -> crate::Result<Self> {
		let Config {
			activity,
			listen: config,
			quic,
			#[cfg(feature = "_certs")]
			tls,
			#[cfg(feature = "websocket")]
			websocket,
			#[cfg(feature = "iroh")]
			iroh,
			publisher,
			subscriber,
			stats,
		} = config;

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

		let versions = config.versions();
		let timeout = config.resolved_timeout();

		// Build a QUIC backend when `--listen` is set, or when nothing else
		// is (the default). A stream-only server (`--listen-unix-bind` with no
		// `--listen`) doesn't also open UDP/443.
		quic.validate()?;

		let build_quic = parts.quic() && (config.bind.is_some() || !config.has_stream_listener());
		// A stream-only server would silently ignore what only QUIC reads. A caller
		// opening only the streams owns QUIC elsewhere, which reads them.
		if parts.quic() && !build_quic {
			config.validate_stream_only()?;
		}
		// Read before the member is taken out below, which consumes `parts`.
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		let build_streams = parts.streams();
		// `parts.quic()` and not `build_quic`: a caller that asked for streams only
		// is not asking for a backend, while leaving everything else configured is
		// still the error it always was.
		#[cfg(not(feature = "quic"))]
		if config.bind.is_some() && parts.quic() {
			return Err(Error::NoBackend("--listen requires the quic feature"));
		}
		#[cfg(not(feature = "quic"))]
		if config.reset.secret.is_some() && parts.quic() {
			return Err(Error::NoBackend("--listen-reset-secret requires the quic feature"));
		}

		// Only the QUIC backend verifies client certificates; the qmux listeners
		// (tcp/unix/websocket) never ask for one, even over `tls://`, so a
		// stream-only server would ignore the CA or pinned peers. A caller opening
		// only the streams owns QUIC elsewhere.
		if parts.quic() && !build_quic && (!config.tls.root.is_empty() || config.tls.peers.is_some()) {
			return Err(Error::MtlsUnsupported);
		}
		if parts.quic() && !config.tls.root.is_empty() {
			#[cfg(not(feature = "quic"))]
			return Err(Error::NoBackend("--listen-tls-root requires the quic feature"));
		}

		#[cfg(feature = "_certs")]
		let tls = {
			if tls.is_some() && config.tls.configured() {
				return Err(crate::tls::Error::ConflictingServe.into());
			}
			#[allow(unused_mut, reason = "only the stream listener features set it")]
			let mut needed = cfg!(feature = "quic") && build_quic;
			#[cfg(feature = "tcp")]
			{
				needed |= build_streams && config.tcp.bind.is_some() && config.tcp.tls == Some(true);
			}
			match (tls, needed) {
				// Mirrors `validate_stream_only`: an identity no listener serves is refused.
				// A streams-only caller owns QUIC elsewhere, which serves it.
				(Some(_), false) if parts.quic() => {
					return Err(Error::NoBackend(
						"server::Config::tls needs a TLS listener (--listen or --listen-tcp-tls)",
					));
				}
				(Some(tls), _) => Some(tls),
				(None, true) => Some(config.tls.load()?),
				(None, false) => None,
			}
		};
		#[cfg(feature = "_certs")]
		if parts.quic() && !build_quic && tls.as_ref().is_some_and(crate::tls::Serve::client_auth) {
			return Err(Error::MtlsUnsupported);
		}

		// The member is a serving handle released by a complete reuseport group,
		// so the backend may take it only once.
		#[cfg(feature = "quic")]
		let member = parts.member();

		#[cfg(feature = "quic")]
		let quic = build_quic
			.then(|| {
				crate::quic::Server::new(crate::quic::Options {
					config: config.clone(),
					quic: quic.clone(),
					member,
					tls: tls.clone().expect("QUIC needs TLS"),
				})
			})
			.transpose()?;

		// Collect the configured stream listeners (at most one TCP, one Unix).
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		let mut stream_binds = Vec::new();
		#[cfg(feature = "tcp")]
		if let Some(addr) = config.tcp.bind.filter(|_| build_streams) {
			let tls = match config.tcp.tls == Some(true) {
				true => Some(TcpTls::new(
					&config,
					&versions,
					#[cfg(feature = "_certs")]
					tls.clone().expect("TLS stream needs an identity"),
				)?),
				false => None,
			};
			stream_binds.push(StreamBind::Tcp(addr, tls));
		}
		#[cfg(all(feature = "uds", unix))]
		if let Some(path) = config.unix.bind.clone().filter(|_| build_streams) {
			stream_binds.push(StreamBind::Unix(path));
		}
		// `None` (or an all-empty allowlist) means the listener enforces nothing.
		#[cfg(all(feature = "uds", unix))]
		let unix_allow = (!config.unix.allow.is_empty()).then(|| config.unix.allow.clone());
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		let streams = StreamListeners::new(
			stream_binds,
			stream_versions(&versions),
			timeout,
			#[cfg(all(feature = "uds", unix))]
			unix_allow,
		);

		// The endpoint was bound before this server's versions were known, so it
		// accepts nothing until they name its ALPNs.
		#[cfg(feature = "iroh")]
		if let Some(endpoint) = iroh.as_ref() {
			iroh::listen(endpoint, &versions)?;
		}

		let mut moq = moq_net::Server::new()
			.with_versions(versions.clone())
			.with_extensions(config.extensions)
			.with_stats(stats);
		if let Some(implementation) = &config.implementation {
			moq = moq.with_implementation(implementation);
		}
		if let Some(publisher) = publisher {
			moq = moq.with_publisher(publisher);
		}
		if let Some(subscriber) = subscriber {
			moq = moq.with_subscriber(subscriber);
		}

		Ok(Server {
			activity,
			#[cfg(any(
				feature = "quic",
				feature = "iroh",
				feature = "websocket",
				feature = "tcp",
				all(feature = "uds", unix)
			))]
			accept: Default::default(),
			moq,
			versions,
			timeout,
			#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
			streams,
			#[cfg(feature = "iroh")]
			iroh,
			#[cfg(feature = "quic")]
			quic,
			#[cfg(feature = "websocket")]
			websocket,
		})
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
		let mut server = self;
		server.moq = server.moq.with_publisher(origin);
		server.serve().await
	}

	/// Accept sessions until the listener stops, ingesting each publisher into `origin`.
	///
	/// The mirror of [`serve_publish`](Self::serve_publish) for the consume direction.
	pub async fn serve_consume(self, origin: moq_net::origin::Producer) -> crate::Result<()> {
		let mut server = self;
		server.moq = server.moq.with_subscriber(origin);
		server.serve().await
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
		let mut server = self;
		server.moq = server.moq.with_publisher(publish).with_subscriber(subscribe);
		server.serve().await
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
			crate::runtime::spawn(crate::panic::isolate(async move {
				if let Err(err) = serve_session(request).await {
					tracing::warn!(%err, "session ended with error");
				}
			}));
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
		#[cfg(feature = "quic")]
		if let Some(quic) = self.quic.as_ref() {
			return quic.certificates();
		}
		// No QUIC backend (e.g. a stream-only `--listen-tcp-bind`): no certificates.
		crate::tls::Certificates::empty()
	}

	#[cfg(not(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		all(feature = "uds", unix)
	)))]
	/// Returns the next partially established session: none, since no compiled
	/// transport is one this server listens on (WebRTC sessions arrive through
	/// [`Listener::accept_webrtc`]).
	async fn accept_next(&mut self) -> Option<Request> {
		None
	}

	/// The accept-loop health of every listener this server owns that performs a real
	/// `accept(2)`: the `tcp`/`unix` stream listeners and, if one was set,
	/// [`Config::websocket`].
	///
	/// Empty on a QUIC-only server, which is the honest answer rather than a
	/// convenient one: a QUIC backend multiplexes every session over one UDP socket,
	/// so it never calls `accept` and has nothing that could fail this way. Publishing
	/// a zero for it would read as a watch that is passing when it can never fire.
	///
	/// Available before [`listen`](Self::listen), so an owner can register these with
	/// a metrics endpoint at startup even though the sockets bind there.
	pub fn accept_health(&self) -> Vec<crate::accept::Health> {
		#[allow(unused_mut, reason = "only the stream listener features extend it")]
		let mut health = Vec::new();
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		health.extend(self.streams.health.iter().cloned());
		#[cfg(feature = "websocket")]
		health.extend(self.websocket.as_ref().map(|ws| ws.accept_health()));
		health
	}

	/// Bind whatever is still unbound and hand back the [`Listener`], without
	/// accepting.
	///
	/// Same terminal bind as [`listen`](Self::listen), including ephemeral ports
	/// via [`Listener::tcp_local_addr`]. Stream accept loops stay stopped until
	/// [`Listener::accept`], so nothing is read off the socket before the caller
	/// is ready to take sessions. [`listen`](Self::listen) is this plus starting
	/// those loops immediately.
	///
	/// A bind failure is the error, not a silent `None` from a later accept. It
	/// leaves nothing bound: the partially built `Listener` drops here, closing
	/// whatever it opened. Build a fresh `Server` from the (cloneable) config to try
	/// again.
	#[allow(
		unused_mut,
		reason = "`mut` is only needed to bind the stream listeners, which a QUIC-only build has none of"
	)]
	pub async fn bind(mut self) -> crate::Result<Listener> {
		crate::runtime::scope(async move {
			#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
			self.streams.bind().await?;
			Ok(Listener { server: self })
		})
		.await
	}

	/// Start serving: bind whatever is still unbound and hand back the
	/// [`Listener`] to accept sessions from.
	///
	/// Terminal, and that is the point: it consumes the `Server`, so the builders
	/// above cannot run afterwards and every session is served the configuration
	/// this call captured. The QUIC socket is bound by [`crate::listen::Config::init`], but
	/// the stream (`tcp`/`unix`) listeners bind asynchronously, so they bind here, and
	/// their accept loops start here too. Use [`bind`](Self::bind) to learn the
	/// port without accepting yet.
	///
	/// A bind failure is the error, not a silent `None` from a later accept. It
	/// leaves nothing bound: the partially built `Listener` drops here, closing
	/// whatever it opened. Build a fresh `Server` from the (cloneable) config to try
	/// again.
	pub async fn listen(self) -> crate::Result<Listener> {
		#[cfg(not(any(feature = "tcp", all(feature = "uds", unix))))]
		{
			self.bind().await
		}
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		{
			let mut listener = self.bind().await?;
			// Accept immediately, so a dial can finish its handshake before the
			// caller polls `accept`. `bind` leaves that until the first accept.
			let server = listener
				.server
				.moq
				.clone()
				.with_versions(listener.server.streams.versions.clone());
			listener.server.streams.serve(&server, &listener.server.activity);
			Ok(listener)
		}
	}

	/// The body of [`Listener::accept`]. Stream accept loops start here if
	/// [`Server::bind`] left them stopped; [`Server::listen`] already started them.
	#[cfg(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		all(feature = "uds", unix)
	))]
	async fn accept_next(&mut self) -> Option<Request> {
		// A `bind` (rather than `listen`) leaves the stream loops stopped so an
		// embedder can read the port before it is willing to take sessions.
		// Starting them here, on the first accept, is that moment.
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		{
			let server = self.moq.clone().with_versions(self.streams.versions.clone());
			self.streams.serve(&server, &self.activity);
		}
		loop {
			// The QUIC endpoint address, reported as a QUIC session's local side.
			#[cfg(feature = "quic")]
			let local = self.local_addr().ok();

			// tokio::select! does not support cfg directives on arms, so we need to create the futures here.
			// `Some(None)` once a configured endpoint stops, which only losing its
			// driver does while we accept.
			#[cfg(feature = "quic")]
			let quic_accept = async {
				#[cfg(feature = "quic")]
				if let Some(quic) = self.quic.as_mut() {
					return Some(quic.accept().await);
				}
				None
			};
			#[cfg(not(feature = "quic"))]
			let quic_accept = async { None::<()> };

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

			#[cfg(feature = "websocket")]
			let ws_ref = self.websocket.as_ref();
			#[cfg(feature = "websocket")]
			let ws_accept = async {
				match ws_ref {
					Some(ws) => Some(ws.accept_pending().await),
					None => None,
				}
			};
			#[cfg(not(feature = "websocket"))]
			let ws_accept = std::future::ready(None::<()>);

			#[allow(unused_variables, reason = "a build without a transport feature never reads it")]
			let setup = Setup {
				activity: self.activity.clone(),
				server: self.moq.clone(),
				timeout: self.timeout,
			};
			#[allow(unused_variables, reason = "a build without a transport feature never reads it")]
			let versions = self.versions.clone();

			// An absent transport resolves `None` rather than parking, so its arm is
			// disabled instead of holding the `select!` open. That is what lets `else`
			// mean "nothing is left that could accept" instead of "the one transport
			// nobody configured is still notionally pending".
			#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
			let stream_accept = self.streams.recv();
			#[cfg(not(any(feature = "tcp", all(feature = "uds", unix))))]
			let stream_accept = std::future::ready(None::<Result<Request, tokio::task::JoinError>>);

			tokio::select! {
				Some(request) = stream_accept => {
					#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
					match request {
						Ok(request) => return Some(request),
						// The process would keep its other listeners and silently drop
						// this one, so stop accepting at all and let the owner fail loudly.
						Err(err) => {
							tracing::error!(%err, "a stream accept loop died");
							self.shutdown().await;
							if err.is_panic() { std::panic::resume_unwind(err.into_panic()); }
							return None;
						}
					}
					#[cfg(not(any(feature = "tcp", all(feature = "uds", unix))))]
					let _ = request;
				}
				Some(_conn) = quic_accept => {
					#[cfg(feature = "quic")]
					{
						// The process would keep its other listeners and silently drop
						// QUIC, so stop accepting at all and let the owner fail loudly.
						let Some(_conn) = _conn else {
							tracing::error!("QUIC endpoint stopped accepting");
							return None;
						};
						let alpns = versions.alpns();
						let deadline = setup.deadline();
						// Closed for drain: refuse so the peer redials elsewhere at once.
						let Some(active) = setup.activity.enter() else {
							_conn.refuse();
							continue;
						};
						self.accept.push(async move {
							// Accept the transport (capturing url + mTLS identity) and exchange the
							// MoQ SETUP up front, so path/role are known before the caller authorizes
							// (like the stream bindings).
							let Accepted { session, url, identity, authority, mut link } = within(deadline, crate::quic::accept(_conn, alpns)).await?;
							link.local = local;
							let address = match url { Some(_) => moq_net::transport::Address::Uri, None => moq_net::transport::Address::Setup };
							let quic = Some((*session).clone());
							let session = crate::transport::Session::new(session, address);
							let (request, deadline) = accept_request(&setup.server, session, deadline).await?;
							// Only WebTransport carries a request URL; raw QUIC puts the path in the SETUP.
							let transport = match url { Some(_) => Transport::WebTransport, None => Transport::Quic };
							Ok(Request { transport, url, identity, authority, link, handshake: Box::new(request), quic, deadline, active })
						}.boxed());
					}
				}
				Some(_conn) = iroh_accept => {
					#[cfg(feature = "iroh")]
					{
						let alpns = versions.alpns();
						let deadline = setup.deadline();
						let Some(active) = setup.activity.enter() else {
							_conn.refuse();
							continue;
						};
						self.accept.push(async move {
							let Accepted { session, url, identity, authority, link } = within(deadline, super::iroh::accept(_conn, alpns)).await?;
							let address = match url { Some(_) => moq_net::transport::Address::Uri, None => moq_net::transport::Address::Setup };
							let session = crate::iroh::Session::new(session, address);
							let (request, deadline) = accept_request(&setup.server, session, deadline).await?;
							Ok(Request {
								transport: Transport::Iroh,
								url,
								identity,
								authority,
								link,
								handshake: Box::new(request),
								#[cfg(feature = "quic")]
								quic: None,
								deadline,
								active,
							})
						}.boxed());
					}
				}
				Some(_pending) = ws_accept => {
					#[cfg(feature = "websocket")]
					{
						// Upgrade and read the SETUP before handing the session over, so a slow
						// peer doesn't stall the accept loop (spawned like the others). A failed
						// `accept(2)` never reaches here, having been classified, counted, and
						// warned about by the listener itself.
						let local = self.websocket_local_addr();
						let deadline = setup.deadline();
						// Dropping the pending upgrade closes its socket.
						let Some(active) = setup.activity.enter() else {
							continue;
						};
						self.accept.push(async move {
							let (session, url, accepted) = within(deadline, _pending).await?;
							let mut options = crate::websocket::Options::new(url, setup.server, active);
							options.link = Link { remote: Some(accepted.remote), local, alpn: Some(accepted.protocol), ..Default::default() };
							accept_websocket(session, options, deadline).await
						}.boxed());
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

	/// The Iroh endpoint from [`Config::iroh`], if one was set.
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
		#[cfg(feature = "quic")]
		if let Some(quic) = self.quic.as_ref() {
			return Ok(quic.local_addr());
		}
		// No QUIC backend (e.g. a stream-only `--listen-tcp-bind`).
		Err(Error::NoBackend("no QUIC listener configured"))
	}

	/// Closes the QUIC connections once this server has been handed away.
	#[cfg(feature = "quic")]
	pub(crate) fn quic_closer(&self) -> Option<crate::quic::Closer> {
		self.quic.as_ref().map(crate::quic::Server::closer)
	}

	/// The address the WebSocket listener from [`Config::websocket`] bound to, if one was set.
	#[cfg(feature = "websocket")]
	pub fn websocket_local_addr(&self) -> Option<net::SocketAddr> {
		self.websocket.as_ref().and_then(|ws| ws.local_addr().ok())
	}

	/// The body of [`Listener::close`].
	async fn shutdown(&mut self) {
		#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
		self.streams.shutdown().await;

		#[cfg(feature = "quic")]
		if let Some(quic) = self.quic.take() {
			quic.shutdown().await;
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

	/// Close the QUIC connections without waiting for the socket, as a drop must.
	fn close(&mut self) {
		#[cfg(feature = "quic")]
		if let Some(quic) = self.quic.as_ref() {
			quic.close();
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
	/// [Request::reject] to complete the handshake.
	///
	/// `None` means nothing can arrive again: every configured listener has stopped
	/// with no handshake in flight. An accept-loop panic closes every listener before
	/// resuming the panic in this call. Everything is already
	/// bound, so a bind failure cannot arrive here. No process signal is watched:
	/// race this against your own ctrl-C future and drop the listener if you want
	/// one to stop the loop.
	pub async fn accept(&mut self) -> Option<Request> {
		crate::runtime::scope(self.server.accept_next()).await
	}

	/// Read the MoQ SETUP from the data channel the remote peer opened on an
	/// [`crate::webrtc::Answerer`], leaving admission to the returned [`Request`].
	///
	/// Waits for ICE and the channel to come up, bounded by the handshake timeout.
	/// The channel must carry the answerer's protocol, a MoQ ALPN this server
	/// speaks. Returns [`Error::Stopped`] once the listener is closing.
	#[cfg(feature = "webrtc")]
	pub async fn accept_webrtc(&self, peer: &crate::webrtc::Answerer) -> crate::Result<Request> {
		crate::runtime::scope(async move {
			use crate::webrtc::Error as WebRtcError;

			let server = &self.server;
			let active = server.activity.enter().ok_or(Error::Stopped)?;
			if !server.versions.alpns().contains(&peer.protocol()) {
				return Err(WebRtcError::Protocol(peer.protocol().to_string()).into());
			}
			let deadline = server.timeout.and_then(|timeout| moq_clock::now().checked_add(timeout));

			let handshake = async {
				let opened = peer.channel().await?;
				if opened.protocol != peer.protocol() {
					return Err(WebRtcError::Protocol(opened.protocol));
				}
				let config = opened.config;
				let protocol = opened.protocol;
				let session = qmux::Session::accept(opened.channel, config).await?;
				Ok((session, protocol))
			};
			let (session, protocol) = within(deadline, handshake).await?;

			let (request, deadline) = accept_request(&server.moq, session, deadline).await?;
			Ok(Request {
				transport: Transport::WebRtc,
				url: None,
				authority: None,
				identity: None,
				link: Link {
					alpn: Some(protocol),
					..Default::default()
				},
				handshake: Box::new(request),
				#[cfg(feature = "quic")]
				quic: None,
				deadline,
				active,
			})
		})
		.await
	}

	/// Close every listener and connection, once each QUIC peer has been sent the
	/// close.
	///
	/// Returns once every bound socket is released, even if accepted sessions are
	/// still held, so the address can be bound again at once. A worker group
	/// member's socket stays in its group until the group is gone.
	pub async fn close(mut self) {
		crate::runtime::scope(self.server.shutdown()).await;
	}

	/// The address the QUIC listener bound to, useful when the config asked for
	/// port 0.
	///
	/// Errors with [`Error::NoBackend`] on a stream-only server, which has no
	/// QUIC listener.
	pub fn local_addr(&self) -> crate::Result<net::SocketAddr> {
		self.server.local_addr()
	}

	/// The address the WebSocket listener from [`Config::websocket`] bound to, if one was set.
	#[cfg(feature = "websocket")]
	pub fn websocket_local_addr(&self) -> Option<net::SocketAddr> {
		self.server.websocket_local_addr()
	}

	/// The address the TCP (qmux) listener bound to, if one was configured.
	#[cfg(feature = "tcp")]
	pub fn tcp_local_addr(&self) -> Option<net::SocketAddr> {
		self.server.streams.tcp_local_addr
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

	/// The Iroh endpoint from [`Config::iroh`], if one was set.
	#[cfg(feature = "iroh")]
	pub fn iroh_endpoint(&self) -> Option<&iroh::Endpoint> {
		self.server.iroh_endpoint()
	}
}

impl Drop for Listener {
	fn drop(&mut self) {
		self.server.close();
	}
}

/// Complete one accepted [`Request`] and wait for the session to close.
async fn serve_session(request: Request) -> crate::Result<()> {
	let session = request.ok().await?;
	Err(session.closed().await.into())
}

/// The MoQ server an accepted connection handshakes against, and how long it has.
#[cfg(any(
	feature = "quic",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	all(feature = "uds", unix)
))]
#[derive(Clone)]
struct Setup {
	activity: moq_net::server::Activity,
	server: moq_net::Server,
	timeout: Option<std::time::Duration>,
}

#[cfg(any(
	feature = "quic",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	all(feature = "uds", unix)
))]
impl Setup {
	/// When a connection accepted now must finish its handshake, or `None` to wait
	/// forever, including past the clock's range.
	fn deadline(&self) -> Option<moq_clock::Instant> {
		moq_clock::now().checked_add(self.timeout?)
	}
}

#[cfg(any(
	feature = "quic",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	all(feature = "uds", unix),
	feature = "webrtc"
))]
/// Read the peer's MoQ SETUP by `deadline`, closing the session with a timeout
/// code if it hasn't arrived. The deadline comes back too, since answering the
/// SETUP is part of the handshake it bounds.
async fn accept_request<S>(
	server: &moq_net::Server,
	session: S,
	deadline: Option<moq_clock::Instant>,
) -> crate::Result<(moq_net::server::Handshake, Option<Deadline>)>
where
	S: moq_net::transport::poll::Boxable + Send + Sync,
{
	// Shared, so the deadline can close the session the server runs.
	let session = std::sync::Arc::new(session);
	let deadline = deadline.map(|at| Deadline::new(at, session.clone()));
	let accept = async { Ok(server.accept_request(moq_net::time::Instant::now(), session).await?) };
	let request = Deadline::bound(deadline.as_ref(), accept).await?;
	Ok((request, deadline))
}

#[cfg(feature = "websocket")]
pub(crate) async fn accept_websocket(
	session: qmux::Session,
	options: crate::websocket::Options,
	deadline: Option<moq_clock::Instant>,
) -> crate::Result<Request> {
	let (request, deadline) = accept_request(&options.server, session, deadline).await?;
	let authority = options.url.host_str().filter(|h| !h.is_empty()).map(str::to_owned);
	Ok(Request {
		transport: Transport::WebSocket,
		url: Some(options.url),
		authority,
		identity: options.identity,
		link: options.link,
		handshake: Box::new(request),
		#[cfg(feature = "quic")]
		quic: None,
		deadline,
		active: options.active,
	})
}

/// When an accepted connection's handshake must be done, and how to tell the peer
/// it wasn't.
struct Deadline {
	at: moq_clock::Instant,
	/// Closes the session with the timeout code.
	close: Box<dyn Fn() + Send + Sync>,
}

impl Deadline {
	#[cfg(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		all(feature = "uds", unix),
		feature = "webrtc"
	))]
	fn new<S>(at: moq_clock::Instant, session: S) -> Self
	where
		S: moq_net::transport::poll::Session + Send + Sync,
	{
		let close = move || {
			let err = moq_net::Error::Timeout;
			moq_net::transport::poll::Session::close(
				&session,
				moq_net::SessionError::from(&err).to_code(),
				&err.to_string(),
			);
		};
		Self {
			at,
			close: Box::new(close),
		}
	}

	/// Finish `handshake` before the deadline, or close the session with the
	/// timeout code. Without a deadline, wait forever.
	async fn bound<T>(deadline: Option<&Self>, handshake: impl Future<Output = crate::Result<T>>) -> crate::Result<T> {
		let Some(deadline) = deadline else {
			return handshake.await;
		};
		match moq_clock::timeout_at(deadline.at, handshake).await {
			Ok(res) => res,
			Err(_) => {
				(deadline.close)();
				Err(moq_net::Error::Timeout.into())
			}
		}
	}
}

/// Finish a transport handshake by `deadline`, or fail with a timeout.
///
/// Nothing is established yet that could carry a close code, so an expired
/// handshake is dropped, which closes its connection.
#[cfg(any(
	feature = "quic",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	all(feature = "uds", unix),
	feature = "webrtc"
))]
async fn within<T, E>(
	deadline: Option<moq_clock::Instant>,
	handshake: impl Future<Output = Result<T, E>>,
) -> crate::Result<T>
where
	Error: From<E>,
{
	let Some(deadline) = deadline else {
		return Ok(handshake.await?);
	};
	match moq_clock::timeout_at(deadline, handshake).await {
		Ok(res) => Ok(res?),
		Err(_) => Err(moq_net::Error::Timeout.into()),
	}
}

/// The version set offered on stream (`tcp://`/`unix://`) listeners.
///
/// A URL-less transport carries the request path in the moq-lite-05+ SETUP, so
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
	/// With its TLS when the listener serves `tls://`.
	#[cfg(feature = "tcp")]
	Tcp(net::SocketAddr, Option<TcpTls>),
	#[cfg(all(feature = "uds", unix))]
	Unix(PathBuf),
}

/// The TLS a `--listen-tcp-tls` listener serves, reloading its certificate
/// while this lives.
#[cfg(feature = "tcp")]
struct TcpTls {
	config: std::sync::Arc<rustls::ServerConfig>,
}

#[cfg(all(feature = "tcp", feature = "_certs"))]
impl TcpTls {
	// qmux does not report peer certificates; keep token-only authentication while
	// using the same certificate resolver as the QUIC listeners.
	fn new(
		_config: &crate::listen::Config,
		versions: &moq_net::Versions,
		serve: crate::tls::Serve,
	) -> crate::Result<Self> {
		let alpn = stream_versions(versions)
			.alpns()
			.iter()
			.map(|alpn| format!("{}{alpn}", qmux::Version::QMux01.prefix()).into_bytes())
			.collect();
		Ok(Self {
			config: serve.anonymous(alpn)?,
		})
	}
}

#[cfg(all(feature = "tcp", not(feature = "_certs")))]
impl TcpTls {
	/// Without a crypto provider there is no certificate to serve.
	fn new(_config: &crate::listen::Config, _versions: &moq_net::Versions) -> crate::Result<Self> {
		Err(Error::NoBackend("--listen-tcp-tls requires a crypto provider feature"))
	}
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
			Self::Tcp(..) => "tcp",
			#[cfg(all(feature = "uds", unix))]
			Self::Unix(_) => "unix",
		}
	}
}

/// The stream (`tcp`/`unix`) listeners owned by a [`Server`].
///
/// Bound by [`Server::bind`] (they bind asynchronously). [`Server::listen`] then starts
/// each accept loop; a bind-only listener starts them on the first
/// [`Listener::accept`] instead. Each loop feeds completed [`Request`]s back over
/// a channel. The tasks own their listeners and in-flight handshakes, and are
/// stopped when the [`Listener`] closes or drops, so no socket lingers.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
struct StreamListeners {
	/// Keeps the TCP listener's TLS certificate reloading while it serves.
	#[cfg(feature = "tcp")]
	_tcp_tls: Option<TcpTls>,
	binds: Vec<StreamBind>,
	/// One per entry in `binds`, in the same order, and created up front rather than
	/// with the listener: an owner registering these with a metrics endpoint does so
	/// at startup, long before the first `accept` binds anything.
	health: Vec<crate::accept::Health>,
	versions: moq_net::Versions,
	timeout: Option<std::time::Duration>,
	#[cfg(all(feature = "uds", unix))]
	unix_allow: Option<crate::unix::Allow>,
	/// Bound sockets whose accept loops have not started. Empty once [`Self::serve`]
	/// runs, and when nothing was configured.
	pending: Vec<BoundListener>,
	/// The address the TCP listener bound, once [`Self::bind`] has run.
	#[cfg(feature = "tcp")]
	tcp_local_addr: Option<net::SocketAddr>,
	rx: Option<tokio::sync::mpsc::Receiver<Request>>,
	tasks: Vec<tokio::task::JoinHandle<std::convert::Infallible>>,
}

#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
impl StreamListeners {
	fn new(
		binds: Vec<StreamBind>,
		versions: moq_net::Versions,
		timeout: Option<std::time::Duration>,
		#[cfg(all(feature = "uds", unix))] unix_allow: Option<crate::unix::Allow>,
	) -> Self {
		let health = binds
			.iter()
			.map(|bind| crate::accept::Health::new(bind.name()))
			.collect();
		Self {
			#[cfg(feature = "tcp")]
			_tcp_tls: None,
			binds,
			health,
			versions,
			timeout,
			#[cfg(all(feature = "uds", unix))]
			unix_allow,
			pending: Vec::new(),
			#[cfg(feature = "tcp")]
			tcp_local_addr: None,
			rx: None,
			tasks: Vec::new(),
		}
	}

	/// Bind every configured listener without accepting.
	///
	/// Everything binds before [`Self::serve`] starts a loop, so a failure
	/// part-way drops the listeners already opened and frees their sockets there
	/// and then, rather than leaving accept loops to be aborted later.
	async fn bind(&mut self) -> crate::Result<()> {
		if self.binds.is_empty() {
			return Ok(());
		}

		let mut pending = Vec::with_capacity(self.binds.len());
		for (bind, health) in self.binds.drain(..).zip(self.health.iter().cloned()) {
			let alpns = self.versions.alpns();
			match bind {
				#[cfg(feature = "tcp")]
				StreamBind::Tcp(addr, tls) => {
					if tls.is_none() && !addr.ip().is_loopback() {
						tracing::warn!(%addr, "tcp listener bound to a non-loopback address; qmux is UNENCRYPTED, ensure the network is trusted");
					}
					let mut listener = crate::tcp::Listener::bind(addr)
						.await?
						.with_protocols(alpns)
						.with_accept_health(health);
					let encrypted = tls.is_some();
					if let Some(tls) = tls {
						listener = listener.with_tls(tls.config.clone());
						self._tcp_tls = Some(tls);
					}
					let local = listener.local_addr()?;
					tracing::info!(addr = %local, tls = encrypted, "listening (tcp)");
					self.tcp_local_addr = Some(local);
					pending.push(BoundListener::Tcp(listener));
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
					pending.push(BoundListener::Unix(listener));
				}
			}
		}
		self.pending = pending;
		Ok(())
	}

	/// Spawn an accept loop for every listener [`Self::bind`] opened.
	///
	/// No-op when nothing is waiting: either nothing was configured, or the loops
	/// are already running. `server` is the configuration each accepted session
	/// handshakes against, fixed by the time this runs.
	fn serve(&mut self, server: &moq_net::Server, activity: &moq_net::server::Activity) {
		if self.pending.is_empty() {
			return;
		}

		let setup = Setup {
			activity: activity.clone(),
			server: server.clone(),
			timeout: self.timeout,
		};
		let (tx, rx) = tokio::sync::mpsc::channel(16);
		for listener in self.pending.drain(..) {
			let task = match listener {
				#[cfg(feature = "tcp")]
				BoundListener::Tcp(listener) => spawn_tcp_loop(listener, setup.clone(), tx.clone()),
				#[cfg(all(feature = "uds", unix))]
				BoundListener::Unix(listener) => spawn_unix_loop(listener, setup.clone(), self.unix_allow.clone(), tx.clone()),
			};
			self.tasks.push(task);
		}
		self.rx = Some(rx);
	}

	/// Yield the next stream [`Request`], or `None` if no listener is running:
	/// either none was configured, or every accept loop has ended. An accept loop
	/// only ends early by panicking, which yields its error.
	async fn recv(&mut self) -> Option<Result<Request, tokio::task::JoinError>> {
		let rx = self.rx.as_mut()?;
		// Never empty: `serve` spawns a loop per listener before setting `rx`.
		let died = futures::future::select_all(self.tasks.iter_mut());
		// The channel closes only once every loop is gone, which can be seen before
		// the dead loop's handle resolves, so wait for that handle to say why.
		let (res, index, _) = tokio::select! {
			Some(request) = rx.recv() => return Some(Ok(request)),
			died = died => died,
		};
		// Stop for good, so a later call never polls the finished handle; `shutdown`
		// still aborts the loops left.
		self.tasks.swap_remove(index);
		self.rx = None;
		match res {
			Ok(never) => match never {},
			Err(err) => Some(Err(err)),
		}
	}

	/// Stop every accept loop and wait until its listener has released the socket.
	async fn shutdown(&mut self) {
		self.binds.clear();
		// Drop sockets that were bound but never accepted, so the address frees
		// even when `run` never started the loops.
		self.pending.clear();
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
	setup: Setup,
	tx: tokio::sync::mpsc::Sender<Request>,
) -> tokio::task::JoinHandle<std::convert::Infallible> {
	let local = listener.local_addr().ok();
	crate::runtime::spawn(async move {
		// Owned by the loop so stopping it (listener close or drop) also aborts
		// every in-flight handshake, releasing its socket.
		let mut handshakes = tokio::task::JoinSet::new();
		loop {
			let pending = tokio::select! {
				pending = listener.accept_pending() => pending,
				Some(_) = handshakes.join_next() => continue,
			};
			// Closed for drain: dropping the pending handshake closes its socket.
			let Some(active) = setup.activity.enter() else {
				continue;
			};
			let deadline = setup.deadline();
			let setup = setup.clone();
			let tx = tx.clone();
			handshakes.spawn(crate::panic::isolate(async move {
				match within(deadline, pending).await {
					Ok((session, remote)) => {
						let link = Link {
							remote: Some(remote),
							local,
							..Default::default()
						};
						stream_request(
							session,
							&setup,
							tx,
							Incoming {
								transport: Transport::Tcp,
								link,
								deadline,
								active,
							},
						)
						.await
					}
					// Per-connection: a failed `accept(2)` is the listener's own to
					// classify and pace, and never surfaces here.
					Err(err) => tracing::warn!(%err, "tcp qmux handshake failed"),
				}
			}));
		}
	})
}

#[cfg(all(feature = "uds", unix))]
fn spawn_unix_loop(
	listener: crate::unix::Listener,
	setup: Setup,
	allow: Option<crate::unix::Allow>,
	tx: tokio::sync::mpsc::Sender<Request>,
) -> tokio::task::JoinHandle<std::convert::Infallible> {
	crate::runtime::spawn(async move {
		// Owned by the loop, as in `spawn_tcp_loop`.
		let mut handshakes = tokio::task::JoinSet::new();
		loop {
			let pending = tokio::select! {
				pending = listener.accept_pending(allow.as_ref()) => pending,
				Some(_) = handshakes.join_next() => continue,
			};
			// Closed for drain: dropping the pending handshake closes its socket.
			let Some(active) = setup.activity.enter() else {
				continue;
			};
			let deadline = setup.deadline();
			let setup = setup.clone();
			let tx = tx.clone();
			handshakes.spawn(crate::panic::isolate(async move {
				match within(deadline, pending).await {
					Ok((session, _cred)) => {
						stream_request(
							session,
							&setup,
							tx,
							Incoming {
								transport: Transport::Unix,
								link: Link::default(),
								deadline,
								active,
							},
						)
						.await;
					}
					// Per-connection, as in `spawn_tcp_loop`.
					Err(err) => tracing::warn!(%err, "unix qmux handshake failed"),
				}
			}));
		}
	})
}

#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
struct Incoming {
	transport: Transport,
	link: Link,
	deadline: Option<moq_clock::Instant>,
	active: moq_net::server::Active,
}

/// Read SETUP in the connection task and forward the resulting request.
#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
async fn stream_request(
	session: qmux::Session,
	setup: &Setup,
	tx: tokio::sync::mpsc::Sender<Request>,
	incoming: Incoming,
) {
	let Incoming {
		transport,
		link,
		deadline,
		active,
	} = incoming;
	match accept_request(&setup.server, session, deadline).await {
		Ok((request, deadline)) => {
			let request = Request {
				transport,
				url: None,
				authority: None,
				identity: None,
				link,
				handshake: Box::new(request),
				#[cfg(feature = "quic")]
				quic: None,
				deadline,
				active,
			};
			let _ = tx.send(request).await;
		}
		Err(err) => tracing::debug!(%err, "stream SETUP handshake failed"),
	}
}

/// The transport-level facts a backend captures while accepting a connection, before the
/// MoQ SETUP. Grouped so the shared accept loop builds a [`Request`] from named fields
/// rather than a wide tuple.
#[cfg(any(feature = "quic", feature = "iroh"))]
pub(crate) struct Accepted<S> {
	pub session: S,
	pub url: Option<Url>,
	pub identity: Option<crate::tls::PeerIdentity>,
	pub authority: Option<String>,
	pub link: Link,
}

/// What the transport knows about an accepted session's link, reported to whoever
/// authorizes it. Every field is optional: a unix socket has no addresses, a qmux
/// stream negotiates no ALPN, and iroh dials by node id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Link {
	/// The peer's socket address.
	pub remote: Option<net::SocketAddr>,
	/// The address the session arrived on.
	pub local: Option<net::SocketAddr>,
	/// The SNI the client presented, when the transport carried TLS.
	pub server_name: Option<String>,
	/// The negotiated application protocol: the TLS ALPN on raw QUIC, the chosen
	/// sub-protocol on WebTransport and WebSocket.
	pub alpn: Option<String>,
}

/// An incoming MoQ session that can be accepted or rejected.
///
/// The transport connection and the MoQ SETUP are already complete, so [`path`](Self::path),
/// [`role`](Self::role), [`url`](Self::url), and [`peer_identity`](Self::peer_identity) are
/// all populated consistently regardless of transport. [Self::with_publisher] and
/// [Self::with_subscriber] configure what is published and subscribed to on the session;
/// otherwise the Server's configuration is used by default. Call [Self::ok] to start the
/// session, or [Self::reject] to reject it (which closes the just-established session).
pub struct Request {
	active: moq_net::server::Active,
	transport: Transport,
	/// The request URL, for transports that carry one (QUIC/WebTransport/WebSocket). `None` for the
	/// URL-less stream bindings, whose request path rides the SETUP instead.
	url: Option<Url>,
	/// The authority the client dialed, when it offered one: the TLS SNI on raw QUIC, the CONNECT
	/// authority on WebTransport. `None` on the URL-less stream bindings and on iroh.
	authority: Option<String>,
	/// The peer's validated mTLS identity, captured at the transport handshake (before
	/// the MoQ SETUP), when the backend supports it.
	identity: Option<crate::tls::PeerIdentity>,
	/// The link facts the transport could see.
	link: Link,
	/// The paused moq-net handshake; the same type whatever the transport.
	handshake: Box<moq_net::server::Handshake>,
	/// The connection under a QUIC or WebTransport request, for [`Request::quic`].
	#[cfg(feature = "quic")]
	quic: Option<crate::quic::driver::connection::Connection>,
	/// What remains of the handshake deadline, which also bounds [`Request::ok`].
	deadline: Option<Deadline>,
}

impl Request {
	/// Reject the session. The transport is already accepted, so this closes the
	/// just-established MoQ session with `code` rather than answering the transport handshake.
	pub async fn reject(self, code: moq_net::SessionError) -> crate::Result<()> {
		self.handshake.close(code.into());
		Ok(())
	}

	/// Publish the given origin to the session.
	pub fn with_publisher(mut self, publish: impl moq_net::Consume<moq_net::origin::Consumer>) -> Self {
		*self.handshake = (*self.handshake).with_publisher(publish);
		self
	}

	/// Subscribe to the given origin from the session.
	pub fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		*self.handshake = (*self.handshake).with_subscriber(subscribe);
		self
	}

	/// Assign the identity this peer's routes are attributed to; see
	/// [`moq_net::server::Handshake::with_peer_hop`]. Derive it from [`Self::peer_identity`],
	/// never from something coarser.
	pub fn with_peer_hop(mut self, hop: moq_net::Hop) -> Self {
		*self.handshake = (*self.handshake).with_peer_hop(hop);
		self
	}

	/// Attach a per-connection [`moq_net::stats::Session`] context to this session.
	pub fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		*self.handshake = (*self.handshake).with_stats(stats);
		self
	}

	/// Set request caps after authorizing the peer and before answering SETUP.
	pub fn with_limits(mut self, limits: moq_net::session::Limits) -> Self {
		*self.handshake = (*self.handshake).with_limits(limits);
		self
	}

	/// The QUIC connection under this request, to change its limits before answering
	/// SETUP. `None` on a transport moq-quic does not drive (qmux streams, iroh).
	#[cfg(feature = "quic")]
	pub fn quic(&self) -> Option<&crate::quic::driver::connection::Connection> {
		self.quic.as_ref()
	}

	/// Finish `work` before what remains of the handshake deadline, or close the
	/// session with the timeout code and return [`moq_net::Error::Timeout`].
	///
	/// Admitting the session is the caller's part of the handshake, so the same
	/// `listen.timeout` that bounds the transport and SETUP bounds it too; a peer
	/// cannot hold a connection open behind a decision that never comes. Without a
	/// deadline, waits forever.
	pub async fn within<T>(&self, work: impl Future<Output = T>) -> crate::Result<T> {
		crate::runtime::scope(Deadline::bound(self.deadline.as_ref(), async { Ok(work.await) })).await
	}

	/// Accept the session, driving it until it closes.
	///
	/// A driver panic is reported through [`Session::closed`].
	///
	/// Bounded by what remains of the handshake deadline: answering SETUP can wait
	/// on a peer that withholds flow-control credit, which keep-alives would
	/// otherwise hold open.
	pub async fn ok(self) -> crate::Result<Session> {
		let Request {
			handshake,
			deadline,
			active,
			..
		} = self;
		// moq-net enforces it, so a reply the peer never took counts in the session's stats.
		let mut handshake = *handshake;
		if let Some(deadline) = deadline {
			handshake = handshake.with_deadline(deadline.at);
		}
		let (session, driver) = crate::runtime::scope(handshake.ok()).await?;
		use tracing::Instrument;
		crate::runtime::spawn(
			async move {
				crate::panic::isolate(async move {
					let _active = active;
					moq_net::time::run(driver).await;
					Ok(())
				})
				.await
				.unwrap_or(Err(Error::Panic))
			}
			.instrument(tracing::Span::current()),
		);
		Ok(session)
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

	/// The host authority the client dialed, or `None` when the client offered none or the
	/// transport carries no host (iroh, stream bindings).
	///
	/// Reported as offered: rustls sends no SNI for an IP-literal dial (RFC 6066),
	/// so the QUIC backend reports `None`.
	///
	/// Not the moq-net IETF SETUP `Authority` parameter. Client-asserted and not authenticated,
	/// so authorize on the token or [`Self::peer_identity`] rather than on this value.
	pub fn authority(&self) -> Option<&str> {
		self.authority.as_deref()
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
		let setup = self.handshake.path();
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
		let setup = self.handshake.path();
		if setup.is_empty() {
			self.url.as_ref().and_then(Url::query)
		} else {
			setup.split_once('?').map(|(_, query)| query)
		}
	}

	/// The peer's socket address, when the transport has one.
	pub fn remote_addr(&self) -> Option<net::SocketAddr> {
		self.link.remote
	}

	/// The address the session arrived on, when the transport has one.
	pub fn local_addr(&self) -> Option<net::SocketAddr> {
		self.link.local
	}

	/// The SNI the client presented, when the transport carried TLS.
	pub fn server_name(&self) -> Option<&str> {
		self.link.server_name.as_deref()
	}

	/// The negotiated application protocol: the TLS ALPN on raw QUIC, the chosen
	/// sub-protocol on WebTransport and WebSocket, `None` where nothing was negotiated.
	pub fn alpn(&self) -> Option<&str> {
		self.link.alpn.as_deref()
	}

	/// The single direction the client advertised in its SETUP, or `None` for a
	/// bidirectional session (it omitted the role, or the version carries none).
	/// Available on every transport. Use it to reject a token that lacks the scope for
	/// the client's intended direction.
	pub fn role(&self) -> Option<moq_net::setup::Role> {
		self.handshake.role()
	}

	/// The session's auth handle, the same one the accepted session's `auth` returns.
	pub fn auth(&self) -> moq_net::auth::Handle {
		self.handshake.auth()
	}

	/// Answer the peer's tokens yourself; see [`moq_net::server::Handshake::requests`].
	pub fn requests(&mut self) -> moq_net::auth::Requests {
		self.handshake.requests()
	}

	/// The Hop ID the peer declared in its SETUP (moq-lite-05+).
	///
	/// A peer declares this when it attaches a publish or subscribe origin.
	/// Older versions and peers without one return `None`.
	///
	/// Self-declared, so treat it as a correlation hint rather than an
	/// authenticated identity: authorize on the token or client certificate.
	pub fn peer_hop(&self) -> Option<moq_net::Hop> {
		self.handshake.peer_hop()
	}

	/// The implementation the client advertised in its SETUP, such as
	/// `moq-net-rs/0.3.10`, or `None` when it advertised none.
	///
	/// Peer-controlled: use it for logs and rollout counts, never to authorize.
	pub fn peer_implementation(&self) -> Option<&str> {
		self.handshake.peer_implementation()
	}

	/// The credential a moq-transport client presented in its SETUP's `AUTHORIZATION
	/// TOKEN` option, unverified. moq-lite sessions return `None`.
	///
	/// Like [`query`](Self::query), it can hold a credential. Avoid logging it.
	pub fn token(&self) -> Option<&moq_net::setup::Token> {
		self.handshake.token()
	}

	/// The client certificate chain the peer presented, if any, validated
	/// against a configured [`crate::tls::Listen::root`] during the handshake.
	///
	/// Captured at the transport handshake (before the SETUP). The QUIC backend supports mTLS;
	/// other transports always return `None`. Use it to grant
	/// elevated access or to close the session once the certificate expires (see
	/// [`crate::tls::PeerIdentity::expiry`]).
	pub fn peer_identity(&self) -> Option<crate::tls::PeerIdentity> {
		self.identity.clone()
	}
}

#[cfg(test)]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod tests {
	use super::*;

	/// A dead loop's channel closes before its handle resolves; that close must not
	/// read as a clean stop, or `accept` would disable the stream arm and hang.
	#[cfg(any(feature = "tcp", all(feature = "uds", unix)))]
	#[tokio::test]
	async fn a_closed_channel_still_reports_the_dead_loop() {
		let mut streams = StreamListeners::new(
			Vec::new(),
			Default::default(),
			None,
			#[cfg(all(feature = "uds", unix))]
			None,
		);
		let (tx, rx) = tokio::sync::mpsc::channel(1);
		drop(tx);
		streams.rx = Some(rx);
		// Not polled until `recv` yields on this single-threaded runtime.
		streams.tasks.push(tokio::spawn(async { panic!("injected") }));

		let died = streams.recv().await;
		assert!(matches!(died, Some(Err(err)) if err.is_panic()));
	}

	#[cfg(all(not(feature = "quic"), feature = "_certs"))]
	#[test]
	fn no_quic_backend_needs_no_served_identity() {
		Server::new(Config::default()).expect("an absent QUIC backend must not load a TLS identity");
	}

	/// A shared identity on a plaintext-only server would be dropped silently.
	#[cfg(all(feature = "tcp", feature = "_certs"))]
	#[tokio::test]
	async fn a_shared_identity_needs_a_tls_listener() {
		let tls = crate::tls::Listen {
			generate: vec!["localhost".into()],
			..Default::default()
		}
		.load()
		.unwrap();
		let mut config = Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		config.tls = Some(tls);
		assert!(matches!(Server::new(config), Err(Error::NoBackend(_))));
	}

	/// A millisecond track: the frames written here carry timestamps.
	#[cfg(all(feature = "uds", unix))]
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// The next route and whether it is active.
	#[cfg(all(feature = "uds", unix))]
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	/// The second handshake must finish while the first peer is still stalled.
	/// Reading the first server bytes proves it reached the handshake before we
	/// connect the second peer; EOF would mean its handshake already failed.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn tcp_stalled_handshake_does_not_block_accept() {
		use futures::FutureExt;
		use tokio::io::AsyncReadExt;
		let activity = moq_net::server::Activity::default();
		let listener = crate::tcp::Listener::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_protocols(["moq-lite-06"]);
		let addr = listener.local_addr().unwrap();
		let (tx, _rx) = tokio::sync::mpsc::channel(1);
		let task = spawn_tcp_loop(
			listener,
			Setup {
				activity: activity.clone(),
				server: moq_net::Server::new(),
				timeout: None,
			},
			tx,
		);
		let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
		stalled.read_u8().await.expect("first handshake started");
		assert!(
			activity.close().now_or_never().is_none(),
			"transport handshake must hold admission"
		);
		let mut remaining = Vec::new();
		tokio::select! {
			biased;
			_ = stalled.read_to_end(&mut remaining) => panic!("first handshake ended before second completed"),
			result = qmux::tcp::connect(addr, { let mut config = qmux::Config::new(qmux::Version::QMux01); config.protocol = qmux::Protocol::Negotiate(vec!["moq-lite-06".into()]); config }) => {
				result.expect("second handshake completes");
			}
		}
		task.abort();
		let _ = task.await;
		activity.close().await;
	}

	/// A drain that closed admission refuses at accept instead of starting a
	/// handshake it would then cut off.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn tcp_closed_activity_refuses_at_accept() {
		let activity = moq_net::server::Activity::default();
		activity.close().await;
		let listener = crate::tcp::Listener::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_protocols(["moq-lite-06"]);
		let addr = listener.local_addr().unwrap();
		let (tx, _rx) = tokio::sync::mpsc::channel(1);
		let task = spawn_tcp_loop(
			listener,
			Setup {
				activity,
				server: moq_net::Server::new(),
				timeout: None,
			},
			tx,
		);
		let result = qmux::tcp::connect(addr, {
			let mut config = qmux::Config::new(qmux::Version::QMux01);
			config.protocol = qmux::Protocol::Negotiate(vec!["moq-lite-06".into()]);
			config
		})
		.await;
		assert!(result.is_err(), "a closed server must not handshake");
		task.abort();
		let _ = task.await;
	}

	/// A refused peer must be closed without exchanging even the qmux preface.
	/// Use our real kernel credentials and change only the configured allowlist.
	#[cfg(all(feature = "uds", unix))]
	#[tokio::test]
	async fn unix_allowlist_precedes_negotiation() {
		use tokio::io::AsyncReadExt;
		let (peer, _other) = tokio::net::UnixStream::pair().unwrap();
		let cred = peer.peer_cred().unwrap();
		for allowed in [false, true] {
			let dir = tempfile::tempdir().unwrap();
			let path = dir.path().join("listener.sock");
			let listener = crate::unix::Listener::bind(&path)
				.await
				.unwrap()
				.with_protocols(["moq-lite-06"]);
			let (tx, _rx) = tokio::sync::mpsc::channel(1);
			let task = spawn_unix_loop(
				listener,
				Setup {
					activity: Default::default(),
					server: moq_net::Server::new(),
					timeout: None,
				},
				Some(crate::unix::Allow {
					uid: vec![if allowed {
						cred.uid()
					} else {
						cred.uid().wrapping_add(1)
					}],
					..Default::default()
				}),
				tx,
			);
			if allowed {
				let mut config = qmux::Config::new(qmux::Version::QMux01);
				config.protocol = qmux::Protocol::Negotiate(vec!["moq-lite-06".into()]);
				let session = qmux::uds::connect(&path, config)
					.await
					.expect("allowed peer negotiates");
				assert_eq!(session.protocol(), Some("moq-lite-06"));
			} else {
				let mut peer = tokio::net::UnixStream::connect(&path).await.unwrap();
				let mut byte = [0];
				let read = peer.read(&mut byte).await;
				// Some kernels report reset instead of EOF when the socket closes.
				assert!(
					matches!(read, Ok(0))
						|| read
							.as_ref()
							.is_err_and(|err| err.kind() == std::io::ErrorKind::ConnectionReset),
					"refused peer reached qmux negotiation: {read:?}, byte={byte:?}"
				);
			}
			task.abort();
			let _ = task.await;
		}
	}

	#[cfg(all(feature = "uds", unix))]
	#[tokio::test]
	async fn unix_stalled_handshake_does_not_block_accept() {
		use futures::FutureExt;
		use tokio::io::AsyncReadExt;
		let activity = moq_net::server::Activity::default();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("listener.sock");
		let listener = crate::unix::Listener::bind(&path)
			.await
			.unwrap()
			.with_protocols(["moq-lite-06"]);
		let (tx, _rx) = tokio::sync::mpsc::channel(1);
		let task = spawn_unix_loop(
			listener,
			Setup {
				activity: activity.clone(),
				server: moq_net::Server::new(),
				timeout: None,
			},
			None,
			tx,
		);
		let mut stalled = tokio::net::UnixStream::connect(&path).await.unwrap();
		stalled.read_u8().await.expect("first handshake started");
		assert!(
			activity.close().now_or_never().is_none(),
			"transport handshake must hold admission"
		);
		let mut remaining = Vec::new();
		tokio::select! {
			biased;
			_ = stalled.read_to_end(&mut remaining) => panic!("first handshake ended before second completed"),
			result = qmux::uds::connect(&path, { let mut config = qmux::Config::new(qmux::Version::QMux01); config.protocol = qmux::Protocol::Negotiate(vec!["moq-lite-06".into()]); config }) => {
				result.expect("second handshake completes");
			}
		}
		task.abort();
		let _ = task.await;
		activity.close().await;
	}

	/// Stopping the accept loop must drop a stalled handshake at once. Each
	/// handshake task holds a `tx` clone, so the channel closes only when they are
	/// all gone. Time pauses only once the socket I/O is done (a paused runtime
	/// waiting on the kernel would skip ahead to qmux's own handshake timeout), so
	/// a leaked task leaves the runtime idle and fires the 1s timeout first.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn tcp_stop_aborts_stalled_handshake() {
		use tokio::io::AsyncReadExt;
		let listener = crate::tcp::Listener::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_protocols(["moq-lite-06"]);
		let addr = listener.local_addr().unwrap();
		let (tx, mut rx) = tokio::sync::mpsc::channel(1);
		let task = spawn_tcp_loop(
			listener,
			Setup {
				activity: Default::default(),
				server: moq_net::Server::new(),
				timeout: None,
			},
			tx,
		);
		let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
		stalled.read_u8().await.expect("handshake started");
		tokio::time::pause();
		task.abort();
		let _ = task.await;
		let closed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await;
		assert!(matches!(closed, Ok(None)), "stalled handshake outlived its accept loop");
	}

	#[cfg(all(feature = "uds", unix))]
	#[tokio::test]
	async fn unix_stop_aborts_stalled_handshake() {
		use tokio::io::AsyncReadExt;
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("listener.sock");
		let listener = crate::unix::Listener::bind(&path)
			.await
			.unwrap()
			.with_protocols(["moq-lite-06"]);
		let (tx, mut rx) = tokio::sync::mpsc::channel(1);
		let task = spawn_unix_loop(
			listener,
			Setup {
				activity: Default::default(),
				server: moq_net::Server::new(),
				timeout: None,
			},
			None,
			tx,
		);
		let mut stalled = tokio::net::UnixStream::connect(&path).await.unwrap();
		stalled.read_u8().await.expect("handshake started");
		tokio::time::pause();
		task.abort();
		let _ = task.await;
		let closed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await;
		assert!(matches!(closed, Ok(None)), "stalled handshake outlived its accept loop");
	}

	/// A qmux session pair over an in-memory WebSocket, so a handshake can run on a
	/// paused clock.
	#[cfg(feature = "websocket")]
	async fn session_pair() -> (qmux::Session, qmux::Session) {
		use qmux::ws::tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

		const ALPN: &str = "qmux-01.moq-lite-06";
		let (client, server) = tokio::io::duplex(64 * 1024);
		let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
		let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
		(
			qmux::ws::Upgraded::new(client, ALPN).connect().unwrap(),
			qmux::ws::Upgraded::new(server, ALPN).accept().unwrap(),
		)
	}

	/// HTTP connection facts and the admission guard survive the upgraded-socket handoff.
	#[cfg(feature = "websocket")]
	#[tokio::test(start_paused = true)]
	async fn upgraded_websocket_preserves_request_metadata() {
		use moq_net::transport::poll::{SendOptions, SendStream as _, Session as _};
		use qmux::ws::tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

		let (client, server) = tokio::io::duplex(64 * 1024);
		let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
		let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
		let activity = moq_net::server::Activity::default();
		let url = "wss://example.com/room?jwt=token".parse().unwrap();
		let mut options = crate::websocket::Options::new(url, moq_net::Server::new(), activity.enter().unwrap());
		options.link = Link {
			remote: Some("127.0.0.1:1234".parse().unwrap()),
			local: Some("127.0.0.1:443".parse().unwrap()),
			server_name: Some("example.com".into()),
			alpn: Some("qmux-01.moq-lite-06".into()),
		};
		options.identity = Some(crate::tls::PeerIdentity::from_chain(Vec::new()));
		let accept = tokio::spawn(crate::websocket::accept(server, options));
		let client = qmux::ws::Upgraded::new(client, "qmux-01.moq-lite-06")
			.connect()
			.unwrap();
		let mut setup = client.open_uni(SendOptions::default()).await.unwrap();
		assert_eq!(setup.write(&[1, 1, 0]).await.unwrap(), 3);
		setup.finish().unwrap();
		let request = accept.await.unwrap().unwrap();
		assert_eq!(request.transport(), Transport::WebSocket);
		assert_eq!(request.path(), "/room");
		assert_eq!(request.query(), Some("jwt=token"));
		assert_eq!(request.authority(), Some("example.com"));
		assert_eq!(request.remote_addr(), Some("127.0.0.1:1234".parse().unwrap()));
		assert_eq!(request.local_addr(), Some("127.0.0.1:443".parse().unwrap()));
		assert_eq!(request.server_name(), Some("example.com"));
		assert_eq!(request.alpn(), Some("qmux-01.moq-lite-06"));
		assert!(request.peer_identity().is_some());
		assert!(activity.close().now_or_never().is_none());
		drop(request);
		assert!(activity.close().now_or_never().is_some());
	}

	/// A scripted peer, recording the code it was closed with.
	///
	/// With `setup` it is a moq-transport peer whose bidi SETUP stream delivers those
	/// bytes and then never grants credit for the reply. Without it, it never opens
	/// anything, while a bidi stream it opens itself records what is written.
	#[cfg(feature = "websocket")]
	#[derive(Clone)]
	struct Scripted {
		alpn: &'static str,
		setup: Option<bytes::Bytes>,
		written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
		closed: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
	}

	#[cfg(feature = "websocket")]
	impl Scripted {
		fn new(alpn: &'static str) -> Self {
			Self {
				alpn,
				setup: None,
				written: Default::default(),
				closed: Default::default(),
			}
		}
	}

	/// The scripted peer never fails, only parks, so its error is never built.
	#[cfg(feature = "websocket")]
	#[derive(Debug, Clone, thiserror::Error)]
	#[error("scripted peer error")]
	struct ScriptedError;

	#[cfg(feature = "websocket")]
	impl web_transport_trait::Error for ScriptedError {
		fn session_error(&self) -> Option<(u32, String)> {
			None
		}
	}

	/// Records every write, or accepts none at all when `stalled`.
	#[cfg(feature = "websocket")]
	struct ScriptedSend {
		written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
		stalled: bool,
	}

	#[cfg(feature = "websocket")]
	impl web_transport_trait::SendStream for ScriptedSend {
		type Error = ScriptedError;

		async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
			if self.stalled {
				std::future::pending::<()>().await;
			}
			self.written.lock().unwrap().extend_from_slice(buf);
			Ok(buf.len())
		}

		fn set_priority(&mut self, _order: i32) {}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}

		fn reset(&mut self, _code: u32) {}

		async fn closed(&mut self) -> Result<(), Self::Error> {
			std::future::pending().await
		}
	}

	#[cfg(feature = "websocket")]
	impl crate::transport::GroupedSend for ScriptedSend {
		type Group = moq_net::transport::poll::Ungrouped;

		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}
	}

	#[cfg(feature = "websocket")]
	impl crate::transport::Grouped for Scripted {
		type Recv = moq_net::transport::poll::Buffered<crate::transport::RecvStream<ScriptedRecv>>;

		fn recv(stream: crate::transport::RecvStream<ScriptedRecv>) -> Self::Recv {
			moq_net::transport::poll::Buffered::new(stream)
		}
	}

	/// Delivers `data` once, then nothing more.
	#[cfg(feature = "websocket")]
	struct ScriptedRecv {
		data: Option<bytes::Bytes>,
	}

	#[cfg(feature = "websocket")]
	impl web_transport_trait::RecvStream for ScriptedRecv {
		type Error = ScriptedError;

		async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
			let Some(mut data) = self.data.take() else {
				return std::future::pending().await;
			};
			let n = dst.len().min(data.len());
			dst[..n].copy_from_slice(&data.split_to(n));
			self.data = (!data.is_empty()).then_some(data);
			Ok(Some(n))
		}

		fn stop(&mut self, _code: u32) {}

		async fn closed(&mut self) -> Result<(), Self::Error> {
			std::future::pending().await
		}
	}

	#[cfg(feature = "websocket")]
	impl web_transport_trait::Session for Scripted {
		type SendStream = ScriptedSend;
		type RecvStream = ScriptedRecv;
		type Error = ScriptedError;

		async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
			std::future::pending().await
		}

		async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			let Some(setup) = self.setup.clone() else {
				return std::future::pending().await;
			};
			let send = ScriptedSend {
				written: Default::default(),
				stalled: true,
			};
			Ok((send, ScriptedRecv { data: Some(setup) }))
		}

		async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			let send = ScriptedSend {
				written: self.written.clone(),
				stalled: false,
			};
			Ok((send, ScriptedRecv { data: None }))
		}

		async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
			std::future::pending().await
		}

		fn send_datagram(&self, _payload: bytes::Bytes) -> Result<(), Self::Error> {
			Ok(())
		}

		async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
			std::future::pending().await
		}

		fn max_datagram_size(&self) -> usize {
			0
		}

		fn protocol(&self) -> Option<&str> {
			Some(self.alpn)
		}

		fn close(&self, code: u32, _reason: &str) {
			self.closed.lock().unwrap().get_or_insert(code);
		}

		async fn closed(&self) -> Self::Error {
			std::future::pending().await
		}
	}

	/// A timeout past the clock's range waits forever instead of panicking on accept.
	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn unreachable_timeout_has_no_deadline() {
		let setup = Setup {
			activity: Default::default(),
			server: moq_net::Server::new(),
			timeout: Some(std::time::Duration::MAX),
		};
		assert_eq!(setup.deadline(), None);
	}

	/// A peer that never sends SETUP is refused at the deadline, and told why.
	#[cfg(feature = "websocket")]
	#[tokio::test(start_paused = true)]
	async fn stalled_setup_is_closed_at_the_deadline() {
		let peer = Scripted::new("moq-lite-06");
		let timeout = std::time::Duration::from_secs(10);
		let setup = Setup {
			activity: Default::default(),
			server: moq_net::Server::new(),
			timeout: Some(timeout),
		};
		let start = tokio::time::Instant::now();
		let Err(err) = accept_request(
			&setup.server,
			crate::transport::Session::new(peer.clone(), moq_net::transport::Address::Uri),
			setup.deadline(),
		)
		.await
		else {
			panic!("a peer that never sent SETUP was accepted");
		};
		assert!(matches!(err, Error::MoqNet(moq_net::Error::Timeout)), "{err}");
		assert_eq!(start.elapsed(), timeout);
		assert_eq!(
			*peer.closed.lock().unwrap(),
			Some(moq_net::SessionError::Timeout.to_code())
		);
	}

	/// Answering SETUP is part of the handshake: a peer that sends its SETUP but
	/// never grants credit for the reply is refused at the same
	/// deadline, rather than held open by keep-alives, and the session counts it.
	#[cfg(feature = "websocket")]
	#[tokio::test(start_paused = true)]
	async fn stalled_setup_reply_is_closed_at_the_deadline() {
		// Record a real CLIENT_SETUP offering only lite-02, whose reply waits for
		// `ok()`; a newer lite version is answered during accept instead. The client
		// then waits on a reply that never comes.
		let recorder = Scripted::new("moql");
		let client = tokio::spawn({
			let recorder = crate::transport::Session::new(recorder.clone(), moq_net::transport::Address::Uri);
			async move {
				let _ = moq_net::Client::new()
					.with_versions("moq-lite-02".parse::<moq_net::Version>().unwrap().into())
					.connect(moq_net::time::Instant::now(), recorder)
					.await;
			}
		});
		tokio::time::sleep(std::time::Duration::from_secs(1)).await;
		client.abort();
		let bytes = std::mem::take(&mut *recorder.written.lock().unwrap());
		assert!(!bytes.is_empty(), "the client never wrote its SETUP");

		let mut peer = Scripted::new("moql");
		peer.setup = Some(bytes.into());
		let timeout = std::time::Duration::from_secs(10);
		let setup = Setup {
			activity: Default::default(),
			server: moq_net::Server::new(),
			timeout: Some(timeout),
		};
		let start = tokio::time::Instant::now();
		let Ok((request, deadline)) = accept_request(
			&setup.server,
			crate::transport::Session::new(peer.clone(), moq_net::transport::Address::Uri),
			setup.deadline(),
		)
		.await
		else {
			panic!("a readable SETUP was refused");
		};
		assert_eq!(start.elapsed(), std::time::Duration::ZERO);

		let registry = moq_net::stats::Registry::new(Default::default());
		let request = Request {
			active: setup.activity.enter().unwrap(),
			transport: Transport::WebSocket,
			url: None,
			authority: None,
			identity: None,
			link: Link::default(),
			handshake: Box::new(request),
			#[cfg(feature = "quic")]
			quic: None,
			deadline,
		}
		.with_stats(
			registry
				.tier(Default::default())
				.session(moq_net::Path::from_static("room")),
		);
		let Err(err) = request.ok().await else {
			panic!("a reply the peer never took completed");
		};
		assert!(matches!(err, Error::MoqNet(moq_net::Error::Timeout)), "{err}");
		assert_eq!(start.elapsed(), timeout);
		assert_eq!(
			*peer.closed.lock().unwrap(),
			Some(moq_net::SessionError::Timeout.to_code())
		);
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(moq_net::stats::Failure::Application), 1, "{failed:?}");
	}

	/// The deadline bounds only the handshake: a session that finished in time
	/// stays open past it.
	#[cfg(feature = "websocket")]
	#[tokio::test(start_paused = true)]
	async fn completed_setup_outlives_the_deadline() {
		use moq_net::transport::poll::Session as _;

		let (client, server) = session_pair().await;
		let timeout = std::time::Duration::from_secs(10);
		let setup = Setup {
			activity: Default::default(),
			server: moq_net::Server::new(),
			timeout: Some(timeout),
		};
		let deadline = setup.deadline();

		let transport = client.clone();
		let connect = tokio::spawn(async move {
			let (session, driver) = moq_net::Client::new()
				.connect(moq_net::time::Instant::now(), transport)
				.await
				.expect("client handshake");
			tokio::spawn(moq_net::time::run(driver));
			session
		});

		let Ok((request, _)) = accept_request(&setup.server, server, deadline).await else {
			panic!("a prompt SETUP was refused");
		};
		let (_session, driver) = request.ok().await.expect("server handshake");
		tokio::spawn(moq_net::time::run(driver));
		let _client = connect.await.unwrap();

		tokio::time::sleep(timeout * 2).await;
		assert!(
			client.closed().now_or_never().is_none(),
			"a completed handshake was closed at its deadline"
		);
	}

	/// A WebSocket peer that never sends its upgrade request holds up only itself.
	/// Real sockets, so this runs on the wall clock.
	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn websocket_stalled_upgrade_does_not_block_accept() {
		let ws = crate::websocket::Listener::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap();
		let addr = ws.local_addr().unwrap();
		let config = Config {
			websocket: Some(ws),
			..Default::default()
		};
		let mut listener = config.init_streams().unwrap().listen().await.unwrap();

		let _stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
		let client = tokio::spawn(async move {
			let session = qmux::ws::Client::new()
				.with_protocol("moq-lite-06", &[qmux::Version::QMux01])
				.connect(&format!("ws://{addr}/"))
				.await
				.expect("upgrade");
			let (_session, driver) = moq_net::Client::new()
				.connect(moq_net::time::Instant::now(), session)
				.await
				.expect("client handshake");
			moq_net::time::run(driver).await
		});

		let request = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
			.await
			.expect("a stalled upgrade blocked the accept loop");
		assert!(request.is_some());
		client.abort();
	}

	#[test]
	fn version_help_lists_every_parseable_name() {
		#[derive(usage_rs::Cli)]
		#[usage(unknown_flags = "error", args_override_self = false)]
		#[usage(settings)]
		struct Cli {
			#[usage(flatten)]
			_listen: crate::listen::Config,
		}

		let help = Cli::render_help(Cli::command(), true).unwrap();
		// Every wire code this crate parses, including the opt-in `-wip` drafts.
		let codes = (0xff0dad00..=0xff0dadff).chain(0xff000000..=0xff0000ff);
		for name in codes
			.filter_map(moq_net::Version::from_code)
			.map(|version| version.as_str())
		{
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
		let server = Config {
			listen: config,
			..Default::default()
		}
		.init()
		.expect("stream-only server");

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

		// A concrete port, so the release is observable, reserved for the whole
		// test so no other process can take it in between. The reservation is a
		// wildcard bind that never listens, with `SO_REUSEADDR`, which Linux and
		// macOS let a loopback listener that also sets it share, as the server's
		// and the final check's both do. Two loopback listeners still collide, so
		// the final check fails while the server holds the port.
		let reserve = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).expect("reserve");
		reserve.set_reuse_address(true).expect("reuse address");
		let wildcard = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0));
		reserve.bind(&wildcard.into()).expect("reserve port");
		let port = reserve
			.local_addr()
			.expect("reserved addr")
			.as_socket()
			.expect("an inet addr")
			.port();
		let live = std::net::TcpListener::bind(("127.0.0.1", port)).expect("the reservation admits a listener");
		std::net::TcpListener::bind(("127.0.0.1", port)).expect_err("a held port fails the final check");
		drop(live);

		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
		config.unix.bind = Some(occupied);
		let server = Config {
			listen: config,
			..Default::default()
		}
		.init()
		.expect("stream-only server");

		assert!(server.listen().await.is_err(), "the unix bind must fail");
		std::net::TcpListener::bind(("127.0.0.1", port)).expect("the tcp port must be free again");
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

		let origin = crate::origin::spawn();
		let broadcast = origin.create_broadcast("test").expect("create broadcast");
		broadcast.announce(Default::default()).expect("announce broadcast");
		let track = broadcast.create_track("video", timed()).expect("create track");
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
		let subscriber = crate::origin::spawn();
		let consumer = subscriber.consume();
		let mut announced = consumer.announced();
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
		let (update, active) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
			.await
			.expect("announce timeout")
			.expect("origin closed");
		assert_eq!(update.prefix.as_str(), "test");
		assert!(active);
		let broadcast = consumer.request("test", None).await.expect("resolve");

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

	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn a_panicked_accept_loop_ends_its_listener() {
		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut listener = Config {
			listen: config,
			..Default::default()
		}
		.init()
		.unwrap()
		.listen()
		.await
		.unwrap();
		let address = listener.tcp_local_addr().unwrap();
		listener
			.server
			.streams
			.tasks
			.push(tokio::spawn(async { panic!("injected accept loop panic") }));
		assert!(crate::panic::isolate(listener.accept()).await.is_none());
		assert!(
			tokio::net::TcpListener::bind(address).await.is_ok(),
			"a dead accept loop left its listener bound"
		);
	}

	/// Closing a listener must release its TCP socket before returning. Reusing it
	/// afterwards is unrepresentable: `listen` consumes the `Server` and `close`
	/// consumes the `Listener`.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn close_releases_stream_listener_socket() {
		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let server = Config {
			listen: config,
			..Default::default()
		}
		.init()
		.expect("stream-only server");
		let listener = server.listen().await.expect("listen");
		let addr = listener.tcp_local_addr().expect("tcp listener bound");
		assert!(tokio::net::TcpListener::bind(addr).await.is_err(), "listener is bound");

		listener.close().await;
		let _rebound = tokio::net::TcpListener::bind(addr)
			.await
			.expect("close must release the listener socket");
	}

	/// Closing a listener must release its UDP socket before returning, even while
	/// an accepted session still holds its QUIC connection, so a restart can rebind
	/// the port at once.
	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn close_releases_quic_socket() {
		let config = crate::listen::Config {
			bind: Some("[::]:0".parse().unwrap()),
			tls: crate::tls::Listen {
				generate: vec!["localhost".into()],
				..Default::default()
			},
			..Default::default()
		};
		let server = config.init(Default::default()).expect("server");
		let mut listener = server.listen().await.expect("listen");
		let addr = listener.local_addr().expect("local addr");

		let origin = crate::origin::spawn();
		let mut client = crate::connect::Config::default();
		client.tls.insecure = Some(true);
		let client = client.init(Default::default()).expect("client").with_publisher(&origin);
		let url: url::Url = format!("moqt://localhost:{}", addr.port()).parse().unwrap();
		let connection = client.with_reconnect(false).connect(url);

		let request = listener.accept().await.expect("no incoming connection");
		let session = request.with_publisher(&origin).ok().await.expect("server handshake");

		listener.close().await;
		std::net::UdpSocket::bind(addr).expect("close must release the QUIC socket");
		drop((session, connection));
	}

	/// A client CA or pinned peers on a stream-only server are refused: no QUIC
	/// listener would verify them, and the stream listeners never ask for a client certificate.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn client_auth_without_a_quic_listener_is_rejected() {
		type Set = fn(&mut crate::listen::Config);
		let cases: [Set; 2] = [
			|c| c.tls.root = vec!["ca.pem".into()],
			|c| c.tls.peers = Some(crate::tls::Peers::new()),
		];
		for set in cases {
			let mut config = crate::listen::Config::default();
			config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
			set(&mut config);

			assert!(matches!(
				config.clone().init(Default::default()),
				Err(Error::MtlsUnsupported)
			));
			// A worker group owning QUIC verifies it, so the streams alone accept it.
			config.init_streams().expect("streams beside worker-owned QUIC");
		}
	}

	/// An explicit QUIC bind cannot be honored without a QUIC backend.
	#[cfg(not(feature = "quic"))]
	#[test]
	fn quic_bind_without_a_quic_backend_is_rejected() {
		let config = crate::listen::Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			..Default::default()
		};

		assert!(matches!(
			Config {
				listen: config,
				..Default::default()
			}
			.init(),
			Err(Error::NoBackend(_))
		));
	}

	/// A reset secret on the default QUIC listener would never be read without a
	/// QUIC backend.
	#[cfg(not(feature = "quic"))]
	#[test]
	fn reset_secret_without_a_quic_backend_is_rejected() {
		let mut config = crate::listen::Config::default();
		config.reset.secret = Some("reset.key".into());

		assert!(matches!(
			Config {
				listen: config,
				..Default::default()
			}
			.init(),
			Err(Error::NoBackend(reason)) if reason.starts_with("--listen-reset-secret")
		));
	}

	/// A client CA on the default QUIC listener names the missing backend, not a
	/// stream-only server the caller never configured.
	#[cfg(not(feature = "quic"))]
	#[test]
	fn client_ca_without_a_quic_backend_is_rejected() {
		let config = crate::listen::Config {
			tls: crate::tls::Listen {
				root: vec!["ca.pem".into()],
				..Default::default()
			},
			..Default::default()
		};

		assert!(matches!(
			Config {
				listen: config,
				..Default::default()
			}
			.init(),
			Err(Error::NoBackend(_))
		));
	}

	/// A QUIC-only server reports nothing. It multiplexes over one UDP socket and
	/// never calls `accept`, so a zero counter would be a watch that cannot fire.
	#[cfg(all(feature = "quic", not(feature = "tcp")))]
	#[test]
	fn accept_health_is_empty_without_a_stream_listener() {
		let server = crate::listen::Config::default()
			.init(Default::default())
			.expect("quic server");
		assert!(server.accept_health().is_empty());
	}

	/// `certificates()` must stay readable with no runtime entered (no guard
	/// escapes to the caller).
	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn certificates_expose_generated_fingerprints() {
		let mut config = crate::listen::Config {
			bind: Some("[::]:0".parse().unwrap()),
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
		let bind: crate::listen::Config = toml::from_str(r#"bind = "[::]:443""#).unwrap();
		assert_eq!(bind.bind.as_ref().map(ToString::to_string).as_deref(), Some("[::]:443"));
		assert!(bind.deprecated().is_empty());

		let bind: crate::listen::Config = toml::from_str(r#"bind = "fly-global-services:443""#).unwrap();
		assert_eq!(
			bind.bind,
			Some(crate::listen::Bind::Host("fly-global-services".to_string(), 443))
		);

		// The released key still parses so the process can name `bind`, but it
		// configures nothing.
		let alias: crate::listen::Config = toml::from_str(r#"listen = "0.0.0.0:4443""#).unwrap();
		assert_eq!(alias.bind, None);
		assert!(
			alias.deprecated().to_string().contains("listen -> bind"),
			"{}",
			alias.deprecated()
		);
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
		assert_eq!(
			config.bind.as_ref().map(ToString::to_string).as_deref(),
			Some("[::]:443")
		);
		assert_eq!(config.unix.bind.as_deref(), Some(std::path::Path::new("/run/moq.sock")));
		assert_eq!(config.unix.allow.uid, vec![1001, 1002]);
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
