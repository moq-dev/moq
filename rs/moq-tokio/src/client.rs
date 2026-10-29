//! Dialing peers: the [`Client`] and the [`Config`] it is built from.
//!
//! [`Config`] pairs the dial half of an endpoint ([`crate::connect::Config`]) with the
//! QUIC settings ([`crate::quic::Config`]) a binary shares with its accept half. The
//! accept side is [`crate::server`].

use crate::connection::Goaway;
use crate::{Addrs, Backoff, Connection, Error, Restart};
#[cfg(all(feature = "websocket", feature = "quic"))]
use std::future::Future;
use url::Url;

/// Everything a [`Client`] is built from.
///
/// Distinct from [`crate::connect::Config`], which is only the dial half of an endpoint:
/// this pairs that half with the [`quic::Config`](crate::quic::Config) a binary shares
/// between dialing and listening, because a `Client` needs both and neither owns the
/// other. Grouping them here is what lets a future knob land as a field rather than as
/// another [`Client::new`] parameter.
///
/// Most callers want the [`crate::connect::Config::init`] shorthand instead.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// The dial side of the endpoint: where to connect and how to be trusted.
	pub connect: crate::connect::Config,

	/// QUIC socket and transport settings, shared with [`crate::Server`].
	pub quic: crate::quic::Config,
}

impl Config {
	/// Build the [`Client`] this config describes.
	pub fn init(self) -> crate::Result<Client> {
		Client::new(self)
	}
}

/// Client for establishing MoQ connections over QUIC, WebTransport, or WebSocket.
///
/// Create via [`crate::connect::Config::init`] or [`Client::new`].
#[derive(Clone)]
pub struct Client {
	pub(crate) moq: moq_net::Client,
	/// The single resolved set of protocol versions, used for credentials and moq ALPNs across
	/// every transport (passed into the QUIC backend's `connect` and used directly for
	/// raw TCP/UDS qmux and WebSocket). Resolved once in [`Client::new`] so the ALPN list
	/// can't diverge between transports.
	pub(crate) versions: moq_net::Versions,
	/// The URL from [`connect.url`](crate::connect::Config::url), dialed by [`Client::publish`] / [`Client::consume`].
	connect: Option<Url>,
	/// The credentials each new connection starts presenting.
	pub(crate) tokens: Vec<moq_net::auth::Token>,
	/// Deadline for one [`Client::connect`], from [`crate::connect::Config::timeout`]. Zero waits forever.
	#[cfg(feature = "_transport")]
	timeout: std::time::Duration,
	pub(crate) reconnect: bool,
	pub(crate) backoff: Backoff,
	pub(crate) goaway: Goaway,
	pub(crate) restart: Restart,
	/// Whether the TLS config pins a certificate fingerprint, which only verifies
	/// the configured host, so a GOAWAY may not redirect elsewhere.
	pub(crate) pinned: bool,
	/// The resolved Happy Eyeballs timings, used by the `tcp://` and `tls://`
	/// dials here; the QUIC backend captures its own copy from the config.
	#[cfg(feature = "tcp")]
	failover_delay: std::time::Duration,
	#[cfg(feature = "tcp")]
	resolution_delay: std::time::Duration,
	/// The TLS settings a `tls://` dial builds from when it runs, so a client
	/// that only dials plaintext `tcp://` never needs a crypto provider.
	#[cfg(feature = "tcp")]
	tcp_tls: crate::tls::Connect,
	#[cfg(feature = "websocket")]
	websocket: crate::websocket::Config,
	/// The TLS server name override used by the WebSocket fallback.
	#[cfg(feature = "websocket")]
	tls_host_name: Option<String>,
	/// Only the TLS-based dials read this; the plaintext qmux transports have none.
	#[cfg(any(feature = "quic", feature = "websocket"))]
	tls: rustls::ClientConfig,
	#[cfg(feature = "quic")]
	quic: Option<crate::quic::Client>,
	#[cfg(feature = "iroh")]
	iroh: Option<crate::iroh::Endpoint>,
	#[cfg(feature = "iroh")]
	iroh_addrs: Vec<std::net::SocketAddr>,
	/// Replaces the transport dial in tests, so the reconnect loop can run over
	/// in-memory sessions on a paused clock.
	#[cfg(test)]
	pub(crate) dialer: Option<std::sync::Arc<Dialer>>,
}

/// A test stand-in for [`Client::dial`].
#[cfg(test)]
pub(crate) type Dialer =
	dyn Fn(crate::connect::Addr) -> std::pin::Pin<Box<dyn Future<Output = crate::Result<Dialed>> + Send>> + Send + Sync;

impl Client {
	/// Build a client from its config.
	///
	/// Errors if no transport feature is compiled in.
	#[cfg(not(feature = "_transport"))]
	pub fn new(_config: Config) -> crate::Result<Self> {
		Err(Error::NoBackend(
			"no backend compiled; enable quic, iroh, websocket, tcp, or uds feature",
		))
	}

	/// Build a client from its config, binding the QUIC socket up front.
	#[cfg(feature = "_transport")]
	pub fn new(config: Config) -> crate::Result<Self> {
		let Config {
			connect: config, quic, ..
		} = config;

		// Refuse here rather than in `init`, so a caller that skipped its own check
		// can't reach a dial that quietly ignored half of what it was given.
		let mut deprecated = config.deprecated();
		deprecated.extend(quic.deprecated());
		if !deprecated.is_empty() {
			return Err(Error::Deprecated(deprecated));
		}

		quic.validate()?;
		config.backoff.validate()?;

		// Only the rustls-backed transports use this. Iroh and the plaintext
		// qmux transports must not require a rustls crypto provider.
		#[cfg(any(feature = "quic", feature = "websocket"))]
		let tls = config.tls.build()?;

		#[cfg(feature = "quic")]
		let quic = Some(crate::quic::Client::new(&config, &quic)?);

		let versions = config.versions();
		// Read before the struct literal below moves fields out of `config`.
		let resolved = config.resolve();
		#[cfg(feature = "tcp")]
		let failover_delay = resolved.race;
		#[cfg(feature = "tcp")]
		let resolution_delay = resolved.resolution_delay;
		#[cfg(feature = "tcp")]
		let tcp_tls = config.tls.clone();
		#[cfg(feature = "websocket")]
		let tls_host_name = config.tls.host_name.clone();
		let timeout = resolved.timeout;

		let mut moq = moq_net::Client::new()
			.with_versions(versions.clone())
			.with_extensions(config.extensions);
		if let Some(implementation) = &config.implementation {
			moq = moq.with_implementation(implementation);
		}

		Ok(Self {
			moq,
			versions,
			connect: config.url,
			tokens: config.tokens,
			timeout,
			reconnect: true,
			backoff: config.backoff,
			goaway: config.goaway,
			restart: Restart::default(),
			pinned: !config.tls.fingerprint.is_empty(),
			#[cfg(feature = "tcp")]
			failover_delay,
			#[cfg(feature = "tcp")]
			resolution_delay,
			#[cfg(feature = "tcp")]
			tcp_tls,
			#[cfg(feature = "websocket")]
			websocket: config.websocket,
			#[cfg(feature = "websocket")]
			tls_host_name,
			#[cfg(any(feature = "quic", feature = "websocket"))]
			tls,
			#[cfg(feature = "quic")]
			quic,
			#[cfg(feature = "iroh")]
			iroh: None,
			#[cfg(feature = "iroh")]
			iroh_addrs: Vec::new(),
			#[cfg(test)]
			dialer: None,
		})
	}

	/// Dial `iroh://` URLs through the given Iroh endpoint.
	///
	/// Required before [`connect`](Self::connect) can serve an `iroh://` URL;
	/// without it those dials fail with [`crate::Error::IrohDisabled`].
	#[cfg(feature = "iroh")]
	pub fn with_iroh(mut self, iroh: crate::iroh::Endpoint) -> Self {
		self.iroh = Some(iroh);
		self
	}

	/// Set direct IP addresses for connecting to iroh peers.
	///
	/// This is useful when the peer's IP addresses are known ahead of time,
	/// bypassing the need for peer discovery (e.g. in tests or local networks).
	#[cfg(feature = "iroh")]
	pub fn with_iroh_addrs(mut self, addrs: Vec<std::net::SocketAddr>) -> Self {
		self.iroh_addrs = addrs;
		self
	}

	/// Publish the given origin to every session this client opens.
	pub fn with_publisher(mut self, publish: impl moq_net::Consume<moq_net::origin::Consumer>) -> Self {
		self.moq = self.moq.with_publisher(publish);
		self
	}

	/// Publish and subscribe through one shared origin.
	pub fn with_origin(mut self, origin: moq_net::origin::Producer) -> Self {
		self.moq = self.moq.with_origin(origin);
		self
	}

	/// Subscribe to the peer's broadcasts, ingesting them into the given origin.
	pub fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		self.moq = self.moq.with_subscriber(subscribe);
		self
	}

	/// Bound what this peer can make a session hold beyond flow control.
	pub fn with_limits(mut self, limits: moq_net::session::Limits) -> Self {
		self.moq = self.moq.with_limits(limits);
		self
	}

	/// Attach a per-connection [`moq_net::stats::Session`] context to all sessions
	/// opened by this client.
	pub fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.moq = self.moq.with_stats(stats);
		self
	}

	/// Share `allocator`'s reservations with every session this client dials.
	pub(crate) fn with_allocator(mut self, allocator: moq_net::bandwidth::Allocator) -> Self {
		self.moq = self.moq.with_allocator(allocator);
		self
	}

	/// Price the links this client dials; see [`moq_net::Client::with_cost`].
	pub fn with_cost(mut self, cost: u64) -> Self {
		self.moq = self.moq.with_cost(cost);
		self
	}

	/// Pin the identity a dialed peer's routes are attributed to, for a peer whose
	/// identity the caller has established; see [`moq_net::Client::with_peer_hop`].
	pub fn with_peer_hop(mut self, hop: moq_net::Hop) -> Self {
		self.moq = self.moq.with_peer_hop(hop);
		self
	}

	/// Override whether this client redials after a session drop.
	///
	/// Defaults to true. Each clone can choose its own reconnect policy.
	pub fn with_reconnect(mut self, reconnect: bool) -> Self {
		self.reconnect = reconnect;
		self
	}

	/// Choose what a GOAWAY naming no URI does. Defaults to [`Restart::Redial`].
	pub fn with_restart(mut self, restart: Restart) -> Self {
		self.restart = restart;
		self
	}

	/// Open a connection to the given peer.
	///
	/// A background task dials, completes the MoQ handshake, and (by default)
	/// redials with exponential backoff whenever the session drops. Wait for the
	/// first session with [`Connection::established`] and for the loop to stop
	/// with [`Connection::closed`]; drop the last clone to stop it. Disable the
	/// redialing with [`Self::with_reconnect`] for
	/// a one-shot dial.
	///
	/// Takes a [`Url`] for the usual case of a peer at a known address. Pass
	/// [`Addrs`] instead when the same peer has several candidate addresses and
	/// only some of them route from here; each attempt walks them in order and
	/// keeps the first that connects.
	pub fn connect(&self, addrs: impl Into<Addrs>) -> Connection {
		Connection::new(self.clone(), addrs.into())
	}

	/// Close every QUIC connection this client dialed, once each peer has been sent the
	/// close.
	///
	/// Clones share one endpoint, so this closes theirs too. Dropping the connections
	/// only queues the close, which nothing sends once the runtime stops: a process
	/// that exits without this leaves each peer waiting out its idle timeout.
	///
	/// This closes at once, discarding stream data the peer has not acknowledged yet.
	/// Call [`Connection::close`] on each connection first to deliver it.
	///
	/// Only the QUIC endpoint is closed. WebSocket, TCP, and UDS sessions end when their
	/// [`Connection`] is dropped (the kernel closes the socket on exit), and an iroh
	/// endpoint passed to `with_iroh` is closed by its owner.
	pub async fn close(self) {
		#[cfg(feature = "quic")]
		if let Some(quic) = self.quic {
			quic.close().await;
		}
	}

	/// Connect to the configured [`connect.url`](crate::connect::Config::url) URL, publishing
	/// `origin` to it.
	///
	/// Returns `None` when no `--connect` URL was configured, so a caller
	/// that may run server-only doesn't have to branch on the URL itself.
	pub fn publish(self, origin: moq_net::origin::Consumer) -> Option<Connection> {
		let url = self.connect.clone()?;
		Some(self.with_publisher(origin).connect(url))
	}

	/// Connect to the configured [`connect.url`](crate::connect::Config::url) URL, consuming its
	/// broadcasts into `origin`.
	///
	/// A session drop closes and unannounces the broadcasts it fed; the reconnect
	/// loop re-announces them once a replacement session attaches. Applications
	/// observe the outage rather than reading from a stale route.
	///
	/// Returns `None` when no `--connect` URL was configured.
	pub fn consume(self, origin: moq_net::origin::Producer) -> Option<Connection> {
		let url = self.connect.clone()?;
		Some(self.with_subscriber(origin).connect(url))
	}

	/// Run a MoQ client session over the data channel an [`crate::webrtc::Offerer`] opened.
	///
	/// Waits for ICE and the channel to come up, bounded by the connect timeout. A
	/// peer link cannot be redialed, so there is no reconnect: the session ends with
	/// the peer connection.
	#[cfg(feature = "webrtc")]
	pub async fn connect_webrtc(&self, peer: &crate::webrtc::Offerer) -> crate::Result<moq_net::Session> {
		crate::runtime::scope(async move {
			let protocol = peer.protocol();
			if !self.versions.alpns().contains(&protocol) {
				return Err(crate::webrtc::Error::Protocol(protocol.to_string()).into());
			}

			let attempt = async {
				let opened = peer.channel().await?;
				let config = opened.config;
				let session = qmux::Session::connect(opened.channel, config)
					.await
					.map_err(crate::webrtc::Error::from)?;
				Ok::<_, Error>(connect_session(&self.moq, session).await?)
			};

			let session = match self.timeout.is_zero() {
				true => attempt.await?,
				false => moq_clock::timeout(self.timeout, attempt)
					.await
					.map_err(|_| Error::ConnectTimeout(self.timeout))??,
			};
			tracing::info!(version = %session.version(), transport = %crate::Transport::WebRtc, "connected");
			Ok(session)
		})
		.await
	}

	/// What a dial to `url` presents of `tokens`, given the versions this client offers.
	/// Dial the given URL and complete the MoQ handshake.
	///
	/// Errors if no transport feature is compiled in.
	#[cfg(not(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	)))]
	pub(crate) async fn dial(
		&self,
		_addr: crate::connect::Addr,
		_presented: &moq_net::auth::Dial,
	) -> crate::Result<Dialed> {
		Err(Error::NoBackend(
			"no backend compiled; enable quic, iroh, websocket, tcp, or uds feature",
		))
	}

	/// Dial the given URL and complete the MoQ handshake, presenting what `presented` decided.
	///
	/// The scheme picks the transport, and `https://` races QUIC against the
	/// WebSocket fallback so a blocked UDP path still connects. The session's
	/// protocol driver is spawned on the current tokio runtime, else the fallback; the session
	/// closes once the last returned handle drops.
	#[cfg(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	pub(crate) async fn dial(
		&self,
		addr: crate::connect::Addr,
		presented: &moq_net::auth::Dial,
	) -> crate::Result<Dialed> {
		let addr = addr.with_url(presented.url.clone());
		#[cfg(test)]
		if let Some(dialer) = &self.dialer {
			return dialer(addr).await;
		}
		let client = match &presented.token {
			Some(token) => {
				let mut client = self.clone();
				client.moq = client.moq.with_token(token.clone());
				std::borrow::Cow::Owned(client)
			}
			None => std::borrow::Cow::Borrowed(self),
		};

		// Each compiled backend adds state to this dispatch future. Keep it off the
		// caller's stack so all-feature builds remain safe on standard 2 MiB threads.
		let attempt = Box::pin(client.connect_inner(addr));

		// The deadline covers the dial AND the handshake, for every transport: it is the
		// only bound some of them have. Dropping `attempt` on expiry cancels whichever
		// arm was still pending.
		let dialed = match self.timeout.is_zero() {
			true => attempt.await?,
			false => match moq_clock::timeout(self.timeout, attempt).await {
				Ok(res) => res?,
				Err(_) => return Err(Error::ConnectTimeout(self.timeout)),
			},
		};

		tracing::info!(version = %dialed.session.version(), transport = %dialed.transport, "connected");
		Ok(dialed)
	}

	#[cfg(any(
		feature = "quic",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	async fn connect_inner(&self, addr: crate::connect::Addr) -> crate::Result<Dialed> {
		let url = addr.url().clone();
		// Each session's transport says whether its own request URI carries the target or
		// the SETUP has to, so every dial below shares this builder.
		let moq = self.moq.clone().with_url(url.clone());

		// Plain TCP (qmux, no TLS). Explicit opt-in scheme; never raced against
		// QUIC, which can't speak it. Use only on a trusted network.
		#[cfg(feature = "tcp")]
		if url.scheme() == "tcp" {
			let session =
				crate::tcp::connect(url, &self.versions.alpns(), self.failover_delay, self.resolution_delay).await?;
			let session = connect_session(&moq, session).await?;
			return Ok(Dialed::new(session, crate::Transport::Tcp));
		}

		// Qmux over TLS over TCP, for links that need neither QUIC nor a WebSocket.
		#[cfg(feature = "tcp")]
		if url.scheme() == "tls" {
			let session = crate::tcp::connect_tls(
				url,
				&self.versions.alpns(),
				&self.tcp_tls,
				self.failover_delay,
				self.resolution_delay,
			)
			.await?;
			let session = connect_session(&moq, session).await?;
			return Ok(Dialed::new(session, crate::Transport::Tcp));
		}

		// Unix domain socket (qmux, no TLS). Same-host only; the server can
		// authenticate us by uid/gid via SO_PEERCRED.
		#[cfg(all(feature = "uds", unix))]
		if url.scheme() == "unix" {
			let session = crate::unix::connect(url, &self.versions.alpns()).await?;
			let session = connect_session(&moq, session).await?;
			return Ok(Dialed::new(session, crate::Transport::Unix));
		}

		// A WebSocket URL names its transport. No QUIC backend can dial it, so there is
		// nothing to race and the fallback's answer is the connect's verdict.
		#[cfg(feature = "websocket")]
		if matches!(url.scheme(), "ws" | "wss") {
			return self.connect_websocket(&moq, addr).await;
		}

		// iroh offers the moq ALPNs ahead of H3, so two moq endpoints normally land on raw
		// QUIC, which carries no request URI. The scheme can't tell us which we got, so the
		// negotiated ALPN decides where the request target travels.
		#[cfg(feature = "iroh")]
		if url.scheme() == "iroh" {
			let endpoint = self.iroh.as_ref().ok_or(Error::IrohDisabled)?;
			let (session, address) =
				crate::iroh::connect(endpoint, url, self.iroh_addrs.iter().copied(), &self.versions).await?;
			let session = connect_session(&moq, crate::iroh::Session::new(session, address)).await?;
			return Ok(Dialed::new(session, crate::Transport::Iroh));
		}

		#[cfg(feature = "quic")]
		if let Some(quic) = self.quic.as_ref() {
			let quic_addr = addr.clone();
			let address = match quic_transport(&url) {
				crate::Transport::Quic => moq_net::transport::Address::Setup,
				_ => moq_net::transport::Address::Uri,
			};
			let quic_handle = async {
				quic.connect(&self.tls, quic_addr, &self.versions)
					.await
					.map(|session| crate::transport::Session::new(session, address))
					.map_err(Error::from)
			};

			#[cfg(feature = "websocket")]
			{
				return self.race_moq_connect(&moq, addr, quic_handle).await;
			}

			#[cfg(not(feature = "websocket"))]
			{
				let session = quic_handle.await?;
				let session = connect_session(&moq, session).await?;
				return Ok(Dialed::new(session, quic_transport(&url)));
			}
		}

		#[cfg(feature = "websocket")]
		return self.connect_websocket(&moq, addr).await;

		#[cfg(not(feature = "websocket"))]
		return Err(Error::NoBackend("no QUIC backend matched; this should not happen"));
	}

	/// Connect over WebSocket alone.
	#[cfg(feature = "websocket")]
	async fn connect_websocket(&self, moq: &moq_net::Client, addr: crate::connect::Addr) -> crate::Result<Dialed> {
		let alpns = self.versions.alpns();
		let session =
			crate::websocket::connect(&self.websocket, &self.tls, self.tls_host_name.as_deref(), addr, &alpns).await?;
		let session = connect_session(moq, session).await?;
		Ok(Dialed::new(session, crate::Transport::WebSocket))
	}

	/// Race the QUIC dial against the WebSocket fallback, handshaking whichever wins.
	/// The loser is cancelled as soon as the winner's transport is up.
	///
	/// Only compiled when there is a QUIC dial to race: a WebSocket-only build connects
	/// over the fallback directly.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	async fn race_moq_connect<Q, S>(
		&self,
		moq: &moq_net::Client,
		addr: crate::connect::Addr,
		quic: Q,
	) -> crate::Result<Dialed>
	where
		Q: Future<Output = crate::Result<S>>,
		S: moq_net::transport::poll::Boxable,
	{
		let url = addr.url().clone();
		let transport = quic_transport(&url);
		let alpns = self.versions.alpns();
		let ws_config = self.websocket.clone();
		let ws_tls = self.tls.clone();
		let ws_tls_host_name = self.tls_host_name.clone();
		let websocket = async move {
			crate::websocket::race_handle(&ws_config, &ws_tls, ws_tls_host_name.as_deref(), addr, &alpns)
				.await
				.map(|res| res.map_err(Error::from))
		};

		match race_transport_connect(&url, quic, websocket).await? {
			TransportRace::Quic(quic) => Ok(Dialed::new(connect_session(moq, quic).await?, transport)),
			TransportRace::WebSocket(session) => {
				let session = connect_session(moq, session).await?;
				Ok(Dialed::new(session, crate::Transport::WebSocket))
			}
		}
	}
}

/// What a QUIC dial to `url` runs on: WebTransport for `https://` (and the `http://`
/// bootstrap), raw QUIC for `moqt://` and `moql://`.
#[cfg(feature = "quic")]
fn quic_transport(url: &Url) -> crate::Transport {
	match url.scheme() {
		"moqt" | "moql" => crate::Transport::Quic,
		_ => crate::Transport::WebTransport,
	}
}

/// A session [`Client::dial`] brought up.
pub(crate) struct Dialed {
	pub session: moq_net::Session,
	/// What `session` runs on.
	pub transport: crate::Transport,
}

impl Dialed {
	#[cfg_attr(
		not(any(
			feature = "quic",
			feature = "iroh",
			feature = "websocket",
			feature = "tcp",
			feature = "uds"
		)),
		allow(dead_code, reason = "only a transport feature dials")
	)]
	fn new(session: moq_net::Session, transport: crate::Transport) -> Self {
		Self { session, transport }
	}
}

/// The arm that won a [`race_transport_connect`]; the other was dropped, cancelling it.
#[cfg(all(feature = "websocket", feature = "quic"))]
enum TransportRace<Q, W> {
	Quic(Q),
	WebSocket(W),
}

/// Race the QUIC dial to `url` against the WebSocket fallback, returning whichever
/// transport comes up first.
#[cfg(all(feature = "websocket", feature = "quic"))]
async fn race_transport_connect<Q, W, QT, WT>(url: &Url, quic: Q, websocket: W) -> crate::Result<TransportRace<QT, WT>>
where
	Q: Future<Output = crate::Result<QT>>,
	W: Future<Output = Option<crate::Result<WT>>>,
{
	tokio::pin!(quic);
	tokio::pin!(websocket);

	let mut quic_err = None;
	let mut websocket_err = None;
	let mut quic_done = false;
	let mut websocket_done = false;

	loop {
		tokio::select! {
			res = &mut quic, if !quic_done => {
				match res {
					Ok(session) => {
						crate::websocket::forget(url);
						return Ok(TransportRace::Quic(session));
					}
					Err(err) => {
						tracing::warn!(%err, "QUIC connection failed");
						quic_err = Some(err);
						quic_done = true;
					}
				}
			}
			res = &mut websocket, if !websocket_done => {
				match res {
					Some(Ok(session)) => return Ok(TransportRace::WebSocket(session)),
					Some(Err(err)) => {
						tracing::warn!(%err, "WebSocket connection failed");
						websocket_err = Some(err);
						websocket_done = true;
					}
					None => {
						websocket_done = true;
					}
				}
			}
			else => break,
		}

		if quic_done && websocket_done {
			break;
		}
	}

	match (quic_err, websocket_err) {
		(Some(quic), Some(websocket)) => Err(race_error(quic, websocket)),
		(Some(err), None) | (None, Some(err)) => Err(err),
		(None, None) => Err(Error::ConnectFailed),
	}
}

/// The error for a race both arms lost.
///
/// Auth is terminal only when both arms refused. A WebTransport-only endpoint
/// answers the fallback with 403 while QUIC is still in flight, and reconnect
/// treats is_auth() as terminal, so a mixed pair reports the retryable error.
#[cfg(all(feature = "websocket", feature = "quic"))]
fn race_error(quic: Error, websocket: Error) -> Error {
	match (quic.is_auth(), websocket.is_auth()) {
		(false, false) => Error::TransportRace {
			quic: std::sync::Arc::new(quic),
			websocket: std::sync::Arc::new(websocket),
		},
		(true, false) => websocket,
		_ => quic,
	}
}

#[cfg(feature = "_transport")]
async fn connect_session<S: moq_net::transport::poll::Boxable>(
	client: &moq_net::Client,
	transport: S,
) -> Result<moq_net::Session, moq_net::Error> {
	let (session, driver) = client.connect(moq_net::time::Instant::now(), transport).await?;
	use tracing::Instrument;
	tokio::spawn(crate::panic::isolate(moq_net::time::run(driver)).instrument(tracing::Span::current()));
	Ok(session)
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use super::*;

	/// A millisecond track: the frames written here carry timestamps.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn fixed_target_preserves_request_and_refuses_redirect() {
		check_fixed_redirect(false, true).await;
	}

	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn one_shot_fixed_target_refuses_redirect() {
		check_fixed_redirect(true, true).await;
	}

	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn unused_fixed_fallback_does_not_restrict_connected_target() {
		check_fixed_redirect(true, false).await;
	}

	#[cfg(feature = "quic")]
	async fn check_fixed_redirect(once: bool, pinned: bool) {
		let mut listen = crate::listen::Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			..Default::default()
		};
		listen.tls.generate = vec!["relay.invalid".into()];
		let server = listen.init(Default::default()).unwrap();
		let mut server = server.listen().await.unwrap();
		let peer = server.local_addr().unwrap();
		let origin = crate::origin::spawn();
		let server_origin = origin.clone();
		let accepted = tokio::spawn(async move {
			let request = server.accept().await.unwrap();
			assert_eq!(request.path(), "/room");
			assert_eq!(request.query(), Some("jwt=secret"));
			let session = request.with_publisher(&server_origin).ok().await.unwrap();
			(server, session)
		});
		let mut config = crate::connect::Config::default();
		config.tls.insecure = Some(true);
		let client = config
			.init(Default::default())
			.unwrap()
			.with_reconnect(!once)
			.with_publisher(&origin);
		let url: url::Url = format!("https://relay.invalid:{}/room?jwt=secret", peer.port())
			.parse()
			.unwrap();
		let target = crate::connect::Addr::pinned(url.clone(), [peer]).unwrap();
		let targets = if pinned {
			crate::connect::Addrs::new(target)
		} else {
			let mut direct = url.clone();
			direct.set_ip_host(peer.ip()).unwrap();
			crate::connect::Addrs::new(direct).or(target)
		};
		let connection = client.connect(targets);
		let connection = connection.established().await.unwrap();
		let (_server, server_session) = accepted.await.unwrap();
		let mut redirect = url;
		if !pinned {
			redirect.set_ip_host(peer.ip()).unwrap();
		}
		server_session
			.drain()
			.send(moq_net::goaway::Goaway::redirect(redirect.to_string()))
			.unwrap();
		let result = connection.closed().await;
		if pinned {
			assert!(matches!(result, Err(crate::Error::PinnedRedirect)));
		} else {
			assert!(result.is_ok());
		}
	}

	/// A parser wrapping the config, since it derives `Args` (a flattened `Parser`
	/// registers an implicit group named after the struct, which collides once two
	/// role configs are flattened together).
	#[derive(usage_rs::Cli)]
	#[usage(unknown_flags = "error", args_override_self = false)]
	#[usage(settings)]
	struct Cli {
		#[usage(flatten)]
		config: crate::connect::Config,
	}

	impl Cli {
		fn config_from<I, T>(args: I) -> crate::connect::Config
		where
			I: IntoIterator<Item = T>,
			T: Into<std::ffi::OsString> + Clone,
		{
			let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
			let args: Vec<_> = args.iter().map(std::ffi::OsString::as_os_str).collect();
			Cli::try_parse_from(&args).expect("valid test arguments").config
		}
	}

	/// A value loaded from TOML survives re-applying CLI args that omit its flag,
	/// one row per value shape.
	#[test]
	fn test_toml_survives_update_from() {
		let default = format!("{:?}", crate::connect::Config::default());
		for toml in [
			"tls.insecure = true",
			r#"resolution_delay = "10ms""#,
			r#"tls.fingerprint = ["abcd1234", "ef567890"]"#,
			r#"version = ["moq-lite-02"]"#,
			r#"url = "https://relay.example.com/anon""#,
			r#"bind = "127.0.0.1:1234""#,
			r#"tls.host_name = "example.host""#,
		] {
			let config: crate::connect::Config = toml::from_str(toml).unwrap();
			let loaded = format!("{config:?}");
			assert_ne!(loaded, default, "{toml} must set a value");

			let mut cli = Cli { config };
			cli.update_from(&[]);
			assert_eq!(format!("{:?}", cli.config), loaded, "{toml}");
		}
	}

	#[test]
	fn test_cli_disable_verify_flag() {
		let config = Cli::config_from(["test", "--connect-tls-insecure"]);
		assert_eq!(config.tls.insecure, Some(true));
	}

	#[test]
	fn test_cli_disable_verify_explicit_false() {
		let config = Cli::config_from(["test", "--connect-tls-insecure=false"]);
		assert_eq!(config.tls.insecure, Some(false));
	}

	/// A released spelling parses into a hidden field and is never read as a
	/// setting: the canonical field stays unset, and the config reports the
	/// migration instead. Honoring it silently is what this replaced, since a
	/// warning leaves the process running on trust settings it dropped.
	#[test]
	fn deprecated_tls_flags_are_reported_not_applied() {
		let config = Cli::config_from(["test", "--tls-disable-verify=true", "--tls-fingerprint", "abcd1234"]);
		assert_eq!(config.tls.insecure, None);
		assert!(config.tls.fingerprint.is_empty());

		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("--tls-disable-verify -> --connect-tls-insecure"),
			"{reported}"
		);
		assert!(
			reported.contains("--tls-fingerprint -> --connect-tls-fingerprint"),
			"{reported}"
		);
	}

	/// The message has to name the environment variable too. A deployment
	/// configured through the environment never typed the flag, so a line naming
	/// only the flag reads as unrelated to why it stopped booting.
	#[test]
	fn the_migration_names_both_spellings() {
		let config = Cli::config_from(["test", "--client-connect", "https://relay.example.com/anon"]);
		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("--client-connect / MOQ_CLIENT_CONNECT -> --connect / MOQ_CONNECT"),
			"{reported}"
		);
	}

	/// A removed CLI policy points embedders at the per-clone API.
	#[test]
	fn removed_reconnect_names_the_native_policy() {
		let config = Cli::config_from(["test", "--client-reconnect=false"]);
		let reported = config.deprecated().to_string();
		assert!(reported.contains("Client::with_reconnect(bool)"), "{reported}");
		assert!(reported.contains("native API only"), "{reported}");
	}

	/// Building a client is the backstop: a caller that skipped its own check must
	/// not reach a dial that ignored half of what it was given.
	#[test]
	fn building_a_client_refuses_a_released_spelling() {
		let config = Cli::config_from(["test", "--client-connect", "https://relay.example.com/anon"]);
		let Err(err) = crate::client::Config {
			connect: config,
			..Default::default()
		}
		.init() else {
			panic!("building a client must refuse a released spelling");
		};
		assert!(matches!(err, Error::Deprecated(_)), "{err}");
		assert!(err.to_string().contains("--connect / MOQ_CONNECT"), "{err}");
	}

	/// Tokens repeat, keep their order, and are refused when empty.
	#[test]
	fn test_connect_tokens() {
		let config = Cli::config_from(["test", "--connect-token", "first", "--connect-token", "second"]);
		let tokens: Vec<&str> = config.tokens.iter().map(moq_net::auth::Token::as_str).collect();
		assert_eq!(tokens, ["first", "second"]);

		let config: crate::connect::Config = toml::from_str(r#"tokens = ["first"]"#).unwrap();
		assert_eq!(config.tokens.len(), 1);
		assert!(toml::from_str::<crate::connect::Config>(r#"tokens = [""]"#).is_err());
	}

	#[test]
	fn test_cli_resolution_delay() {
		let config = Cli::config_from(["test", "--connect-resolution-delay", "0s"]);
		assert_eq!(config.resolution_delay, Some(std::time::Duration::ZERO.into()));
		assert_eq!(config.resolve().resolution_delay, std::time::Duration::ZERO);
	}

	#[test]
	fn test_toml_fingerprint_accepts_single_string() {
		let toml = r#"
			tls.fingerprint = "abcd1234"
		"#;

		let config: crate::connect::Config = toml::from_str(toml).unwrap();
		assert_eq!(config.tls.fingerprint, vec!["abcd1234"]);
	}

	#[test]
	fn test_cli_version_help_lists_every_parseable_name() {
		let help = Cli::render_help(Cli::command(), true).expect("long help");
		// Every wire code this crate parses, including the opt-in `-wip` drafts.
		let codes = (0xff0dad00..=0xff0dadff).chain(0xff000000..=0xff0000ff);
		for name in codes
			.filter_map(moq_net::Version::from_code)
			.map(|version| version.as_str())
		{
			assert!(help.contains(name), "missing {name} from --connect-version help");
		}
	}

	/// A removed TOML policy still parses so it can name the per-clone native API.
	#[test]
	fn test_toml_reconnect_is_reported_as_native_policy() {
		let config: crate::connect::Config = toml::from_str("reconnect = false").unwrap();

		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("reconnect -> Client::with_reconnect(bool)"),
			"{reported}"
		);
		assert!(reported.contains("native API only"), "{reported}");
	}

	/// An explicit canonical bind remains distinguishable from an omitted bind.
	#[test]
	fn test_cli_bind_prefers_canonical() {
		let config = Cli::config_from(["test"]);
		assert_eq!(config.bind, None, "unset means the default");
		assert_eq!(config.resolve().bind, "[::]:0".parse().unwrap());

		let config = Cli::config_from(["test", "--connect-bind", "[::]:0"]);
		assert_eq!(
			config.bind,
			Some("[::]:0".parse().unwrap()),
			"an explicit bind is kept even when it is the default"
		);
	}

	/// Several versions offered at once, which the canonical flag takes by repeating.
	#[test]
	fn test_version_list() {
		let config = Cli::config_from([
			"test",
			"--connect-version",
			"moq-lite-03",
			"--connect-version",
			"moq-lite-02",
		]);
		assert_eq!(
			config.version,
			vec![
				"moq-lite-03".parse::<moq_net::Version>().unwrap(),
				"moq-lite-02".parse::<moq_net::Version>().unwrap()
			]
		);
	}

	#[test]
	fn test_cli_no_version_defaults_to_all() {
		let config = Cli::config_from(["test"]);
		assert!(config.version.is_empty());
		// versions() helper returns all when none specified
		assert_eq!(config.versions().alpns().len(), moq_net::ALPNS.len());
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn race_transport_connect_keeps_websocket_after_quic_auth_error() {
		let quic = async { Err::<usize, _>(crate::ConnectError::Unauthorized.into()) };
		let websocket = async {
			// This only needs to complete later than the immediately ready QUIC auth error.
			tokio::task::yield_now().await;
			Some(Ok(1usize))
		};

		let value = super::race_transport_connect(&race_url(), quic, websocket)
			.await
			.unwrap();
		assert!(matches!(value, super::TransportRace::WebSocket(1)));
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn race_transport_connect_keeps_quic_after_websocket_forbidden() {
		let quic = async {
			tokio::task::yield_now().await;
			Ok(3usize)
		};
		let websocket = async { Some(Err::<usize, _>(crate::ConnectError::Forbidden.into())) };

		let value = super::race_transport_connect(&race_url(), quic, websocket)
			.await
			.unwrap();
		assert!(matches!(value, super::TransportRace::Quic(3)));
	}

	/// A WebTransport-only endpoint answers the WebSocket fallback with 403 while the
	/// QUIC dial is still in flight. One transport being refused is not the connect's
	/// verdict: QUIC finishes the race and the session comes up.
	///
	/// Inline rather than in `tests/` so each arm dials its own ephemeral port: the
	/// public connect sends the fallback to the QUIC port, which nothing reserves over
	/// TCP as well.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn websocket_forbidden_does_not_end_a_quic_connect() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		let listener = tokio::net::TcpListener::bind("[::]:0").await.unwrap();
		let ws_port = listener.local_addr().unwrap().port();
		let forbid = tokio::spawn(async move {
			let (mut stream, _) = listener.accept().await?;
			let mut buf = [0; 1024];
			let _ = stream.read(&mut buf).await?;
			stream
				.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
				.await?;
			Ok::<_, std::io::Error>(())
		});

		let mut listen = crate::listen::Config {
			bind: Some("[::]:0".parse().unwrap()),
			..Default::default()
		};
		listen.tls.generate = vec!["localhost".into()];
		let mut server = listen.init(Default::default()).unwrap().listen().await.unwrap();
		let quic_port = server.local_addr().unwrap().port();
		let origin = crate::origin::spawn();
		let accepted = tokio::spawn(async move {
			let request = server.accept().await.expect("no incoming connection");
			request
				.with_publisher(&origin)
				.ok()
				.await
				.map(|session| (server, session))
		});

		let mut config = crate::connect::Config::default();
		config.tls.insecure = Some(true);
		// No head start, so the fallback dials while QUIC waits on the 403.
		config.websocket.delay = Some((std::time::Duration::ZERO).into());
		let client = config.init(Default::default()).unwrap();

		// The same race `connect_inner` runs, except the fallback dials its own port,
		// as plain ws:// so the listener above can answer without TLS.
		let quic = client.quic.clone().unwrap();
		let (tls, versions) = (client.tls.clone(), client.versions.clone());
		let quic_addr: crate::connect::Addr = Url::parse(&format!("https://localhost:{quic_port}")).unwrap().into();
		let ws_addr: crate::connect::Addr = Url::parse(&format!("http://localhost:{ws_port}")).unwrap().into();
		// Hold QUIC until the fallback has been refused, so the 403 is always exercised.
		let quic = Box::pin(async move {
			forbid.await.unwrap().expect("fallback listener failed");
			quic.connect(&tls, quic_addr, &versions)
				.await
				.map(|session| crate::transport::Session::new(session, moq_net::transport::Address::Uri))
				.map_err(Error::from)
		});

		let dialed = tokio::time::timeout(
			std::time::Duration::from_secs(10),
			client.race_moq_connect(&client.moq, ws_addr, quic),
		)
		.await
		.expect("client connect timed out")
		.expect("a fallback refused on auth must not end a connect whose QUIC arm succeeds");

		assert_eq!(dialed.transport, crate::Transport::WebTransport);
		drop(dialed);
		accepted.await.unwrap().expect("server handshake failed");
	}

	/// QUIC wins while the fallback handshake is held until QUIC connects.
	/// Each transport owns its ephemeral port; this exercises the same race as
	/// connect_inner without depending on which real handshake runs faster.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tracing_test::traced_test]
	#[tokio::test]
	async fn broadcast_race_quic_wins() {
		let pub_origin = crate::origin::spawn();
		let broadcast = pub_origin.create_broadcast("test").expect("failed to create broadcast");
		broadcast
			.announce(Default::default())
			.expect("failed to create broadcast");
		let track = broadcast
			.create_track("video", timed())
			.expect("failed to create track");
		let mut group = track.append_group().expect("failed to append group");
		group
			.write_frame(crate::moq_net::Timestamp::ZERO, b"hello".as_ref())
			.expect("failed to write frame");
		group.finish().expect("failed to finish group");

		let ws_listener = crate::websocket::Listener::bind("[::]:0".parse().unwrap())
			.await
			.expect("failed to bind WebSocket listener");
		let ws_port = ws_listener.local_addr().expect("failed to get ws addr").port();
		let (connected, quic_connected) = tokio::sync::oneshot::channel();
		let websocket_handle = tokio::spawn(async move {
			quic_connected.await.expect("QUIC connected");
			ws_listener.accept().await
		});

		let mut server_config = crate::listen::Config {
			bind: Some("[::]:0".parse().unwrap()),
			..Default::default()
		};
		server_config.tls.generate = vec!["localhost".into()];
		let server = server_config.init(Default::default()).expect("failed to init server");
		let mut server = server.listen().await.expect("failed to listen");
		let quic_port = server.local_addr().expect("failed to get QUIC addr").port();

		let sub_origin = crate::origin::spawn();
		let sub_consumer = sub_origin.consume();
		let mut announcements = sub_consumer.announced();
		let mut client_config = crate::connect::Config::default();
		client_config.tls.insecure = Some(true);
		// Dial both arms immediately; the fallback handshake is gated by QUIC.
		client_config.websocket.delay = Some((std::time::Duration::ZERO).into());

		let client = client_config.init(Default::default()).expect("failed to init client");
		let quic_addr: crate::connect::Addr = Url::parse(&format!("https://localhost:{quic_port}")).unwrap().into();
		let ws_addr: crate::connect::Addr = Url::parse(&format!("http://localhost:{ws_port}")).unwrap().into();

		// Keep this aligned with the newest default Lite version, as in tests/integration/broadcast.rs.
		let expected_version: moq_net::Version = "moq-lite-06".parse().expect("invalid version");

		let server_handle = tokio::spawn(async move {
			let request = server.accept().await.expect("no incoming connection");
			assert_eq!(
				request.transport(),
				crate::Transport::WebTransport,
				"expected the QUIC listener",
			);
			let session = request.with_publisher(&pub_origin).ok().await?;
			assert_eq!(session.version(), expected_version, "server negotiated stale version");
			let _broadcast = broadcast;
			let _track = track;
			let _ = session.closed().await;
			Ok::<_, anyhow::Error>(())
		});

		let client = client.with_subscriber(sub_origin);
		let dialer = client.clone();
		let quic = Box::pin(async move {
			let quic = dialer.quic.as_ref().expect("QUIC backend");
			let session = quic.connect(&dialer.tls, quic_addr, &dialer.versions).await?;
			connected.send(()).expect("fallback listener alive");
			Ok::<_, crate::Error>(crate::transport::Session::new(
				session,
				moq_net::transport::Address::Uri,
			))
		});
		let cc = client
			.race_moq_connect(&client.moq, ws_addr, quic)
			.await
			.expect("client connect failed");

		assert_eq!(
			cc.session.version(),
			expected_version,
			"client negotiated stale version"
		);
		websocket_handle.abort();
		let _ = websocket_handle.await;

		let update = match announcements.next().await.expect("origin closed") {
			moq_net::announce::Event::Start(update) => update,
			event => panic!("expected announcement, got {event:?}"),
		};
		assert_eq!(update.prefix.as_str(), "test");
		let broadcast = sub_consumer.request("test", None).await.expect("broadcast resolves");
		let mut track = broadcast
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = track.recv_group().await.expect("receive group").expect("track open");
		let frame = group.read_frame().await.expect("read frame").expect("group open");
		assert_eq!(&frame.payload[..], b"hello");

		drop(cc);
		server_handle
			.await
			.expect("server task panicked")
			.expect("server task failed");
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn race_transport_connect_reports_auth_when_both_refuse() {
		let quic = async { Err::<usize, _>(crate::ConnectError::Unauthorized.into()) };
		let websocket = async { Some(Err::<usize, _>(crate::ConnectError::Forbidden.into())) };

		let Err(err) = super::race_transport_connect(&race_url(), quic, websocket).await else {
			panic!("the race must fail");
		};
		assert!(err.is_auth(), "unexpected error: {err}");
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn race_transport_connect_reports_quic_error_when_websocket_forbidden() {
		let quic = async {
			tokio::task::yield_now().await;
			Err::<usize, _>(Error::ConnectFailed)
		};
		let websocket = async { Some(Err::<usize, _>(crate::ConnectError::Forbidden.into())) };

		let Err(err) = super::race_transport_connect(&race_url(), quic, websocket).await else {
			panic!("the race must fail");
		};
		assert!(matches!(err, Error::ConnectFailed), "unexpected error: {err}");
		assert!(!err.is_auth(), "mixed auth/non-auth must stay retryable: {err}");
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn race_transport_connect_keeps_websocket_after_quic_non_auth_error() {
		let quic = async { Err::<usize, _>(Error::ConnectFailed) };
		let websocket = async { Some(Ok(7usize)) };

		let value = super::race_transport_connect(&race_url(), quic, websocket)
			.await
			.unwrap();
		assert!(matches!(value, super::TransportRace::WebSocket(7)));
	}

	/// The URL a race unit test dials. Each test runs in its own process under nextest,
	/// so the process-wide [`crate::websocket::won`] memo starts empty.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	fn race_url() -> Url {
		Url::parse("https://race.invalid/").unwrap()
	}

	/// QUIC winning a race clears WebSocket's earlier win, so the next dial gives QUIC
	/// its head start again.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test(start_paused = true)]
	async fn quic_winning_restores_its_head_start() {
		crate::websocket::remember(&race_url());
		let quic = async { Ok("quic") };
		let (websocket, _dropped) = loser::<Option<crate::Result<&str>>>();

		let won = super::race_transport_connect(&race_url(), quic, websocket)
			.await
			.unwrap();
		assert!(matches!(won, super::TransportRace::Quic("quic")));
		assert!(
			!crate::websocket::won(&race_url()),
			"QUIC won, yet the next dial still skips its head start"
		);
	}

	/// A race arm that never finishes, and reports through `dropped` once it is dropped.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	fn loser<T>() -> (
		impl Future<Output = T>,
		tokio::sync::oneshot::Receiver<std::convert::Infallible>,
	) {
		let (alive, dropped) = tokio::sync::oneshot::channel();
		let arm = async move {
			let _alive = alive;
			std::future::pending().await
		};
		(arm, dropped)
	}

	/// WebSocket winning cancels the QUIC dial before the race returns, so no QUIC
	/// session can come up behind the one WebSocket serves.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test(start_paused = true)]
	async fn websocket_winning_cancels_the_quic_dial() {
		let (quic, mut dropped) = loser::<crate::Result<&str>>();
		let websocket = async { Some(Ok("websocket")) };

		let won = super::race_transport_connect(&race_url(), quic, websocket)
			.await
			.unwrap();
		assert!(matches!(won, super::TransportRace::WebSocket("websocket")));
		assert_eq!(
			dropped.try_recv(),
			Err(tokio::sync::oneshot::error::TryRecvError::Closed),
			"the QUIC dial outlived WebSocket's win"
		);
	}

	/// QUIC winning cancels the WebSocket dial before the race returns.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test(start_paused = true)]
	async fn quic_winning_cancels_the_websocket_dial() {
		let quic = async { Ok("quic") };
		let (websocket, mut dropped) = loser::<Option<crate::Result<&str>>>();

		let won = super::race_transport_connect(&race_url(), quic, websocket)
			.await
			.unwrap();
		assert!(matches!(won, super::TransportRace::Quic("quic")));
		assert_eq!(
			dropped.try_recv(),
			Err(tokio::sync::oneshot::error::TryRecvError::Closed),
			"the WebSocket dial outlived QUIC's win"
		);
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn race_transport_connect_returns_when_quic_transport_connects() {
		let quic = async { Ok("quic") };
		let websocket = std::future::pending::<Option<crate::Result<&str>>>();

		let value = tokio::time::timeout(
			std::time::Duration::from_secs(1),
			super::race_transport_connect(&race_url(), quic, websocket),
		)
		.await
		.expect("race waited for WebSocket after QUIC transport connected")
		.unwrap();
		assert!(matches!(value, super::TransportRace::Quic("quic")));
	}

	/// Iroh carries no rustls state, so constructing its client must not require an
	/// application-installed crypto provider.
	#[cfg(all(feature = "iroh", not(any(feature = "quic", feature = "websocket"))))]
	#[test]
	fn iroh_only_client_does_not_require_a_tls_provider() {
		crate::connect::Config::default().init().expect("iroh-only client");
	}

	/// A peer that completes the TCP handshake and then never speaks: the QUIC arm
	/// gives up on its own, but the WebSocket arm has no deadline of its own, so the
	/// race stays pending forever. Without the connect timeout this test hangs.
	///
	/// That is what wedged a publisher against a livelocked relay: [`Connection`] only
	/// re-arms its backoff (and checks its give-up timeout) *between* attempts, so an
	/// attempt that never returns stalls the retry loop for good.
	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn connect_times_out_against_a_peer_that_never_speaks() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();

		let timeout = crate::connect::DEFAULT_TIMEOUT;
		let mut config = crate::connect::Config {
			timeout: Some((timeout).into()),
			..Default::default()
		};
		config.websocket.delay = Some((std::time::Duration::ZERO).into());
		let client = config.init(Default::default()).unwrap();

		// Nothing is listening on UDP, so the QUIC arm fails and leaves the WebSocket
		// arm alone against the silent peer.
		let url: Url = format!("https://127.0.0.1:{}/", addr.port()).parse().unwrap();

		// `dial` rather than `connect`: this is about one attempt's deadline, and
		// `connect` now hands back a reconnect loop that would redial past it.
		let presented = moq_net::auth::Tokens::new(Vec::new()).dial(&url, &client.versions);
		let mut attempt = Box::pin(client.dial(url.into(), &presented));
		let _silent = tokio::select! {
			res = &mut attempt => match res {
				Err(err) => panic!("connect failed before the silent peer accepted it: {err}"),
				Ok(_) => panic!("connected to a peer that never spoke"),
			},
			res = listener.accept() => res.unwrap().0,
		};

		// Freeze only after TCP connected, then advance directly to the deadline. The
		// accepted socket stays in scope and silent until the attempt returns.
		tokio::time::pause();
		tokio::time::advance(timeout).await;

		let err = match attempt.await {
			Err(err) => err,
			Ok(_) => panic!("connected to a peer that never spoke"),
		};

		assert!(matches!(err, Error::ConnectTimeout(_)), "unexpected error: {err}");
	}
}
