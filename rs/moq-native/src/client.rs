use crate::{Backoff, Error, QuicBackend, Reconnect};
#[cfg(feature = "websocket")]
use std::future::Future;
use std::net;
use url::Url;

const DEFAULT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Configuration for the MoQ client.
#[derive(Clone, Debug, clap::Parser, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ClientConfig {
	/// The URL to dial.
	///
	/// Supports WebTransport (`https`/`http`), WebSocket (`ws`/`wss`), raw QUIC
	/// (`moqt`/`moql`), qmux over `tcp`/`unix`, and `iroh`. The URL path is the
	/// request/auth path (e.g. `/anon` for a public relay) and `?jwt=` supplies a
	/// token. `http://` first fetches `/certificate.sha256` for the (insecure)
	/// self-signed fingerprint; `https://` connects directly.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "client-connect", long = "client-connect", env = "MOQ_CLIENT_CONNECT")]
	pub connect: Option<Url>,

	/// Listen for UDP packets on the given address.
	#[arg(
		id = "client-bind",
		long = "client-bind",
		default_value = "[::]:0",
		env = "MOQ_CLIENT_BIND"
	)]
	pub bind: net::SocketAddr,

	/// The QUIC backend to use.
	/// Auto-detected from compiled features if not specified.
	#[arg(id = "client-backend", long = "client-backend", env = "MOQ_CLIENT_BACKEND")]
	pub backend: Option<QuicBackend>,

	/// Delay before also dialing the next resolved address (Happy Eyeballs).
	///
	/// When DNS returns multiple addresses, attempts alternate between IPv6 and
	/// IPv4, each starting this long after the previous one (or immediately when
	/// it fails), and the first connection to complete wins. `0s` dials every
	/// address at once. Defaults to 250ms. Applies to the QUIC and `tcp://` dials.
	///
	/// This staggers the attempts within one [`Client::connect`]; [`Self::timeout`]
	/// bounds that call as a whole.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "client-failover-delay",
		long = "client-failover-delay",
		env = "MOQ_CLIENT_FAILOVER_DELAY",
		value_parser = humantime::parse_duration,
	)]
	pub failover_delay: Option<std::time::Duration>,

	/// Maximum time for one [`Client::connect`], covering the dial and the MoQ
	/// handshake. Defaults to 30 seconds; set to 0 to wait forever.
	///
	/// This has to live above the transports rather than inside one: QUIC bounds its
	/// own dial, but the WebSocket fallback and the handshake that follows either
	/// transport have no deadline of their own, so a peer that accepts TCP and then
	/// never speaks would hang the whole connect. [`Client::reconnect`] only re-arms
	/// its backoff between attempts, so an attempt that never returns stalls the
	/// retry loop indefinitely.
	#[arg(
		id = "client-connect-timeout",
		long = "client-connect-timeout",
		env = "MOQ_CLIENT_CONNECT_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	pub timeout: Option<std::time::Duration>,

	/// QUIC transport tuning (`--client-quic-*`): stream limits, GSO, timeouts.
	#[command(flatten)]
	#[serde(default)]
	pub quic: crate::quic::Client,

	/// Restrict the client to specific MoQ protocol version(s).
	///
	/// By default, the client offers all supported versions and lets the server choose.
	/// Use this to force a specific version, e.g. `--client-version moq-lite-02`.
	/// Can be specified multiple times to offer a subset of versions.
	///
	/// Valid values: moq-lite-01, moq-lite-02, moq-lite-03, moq-transport-14, moq-transport-15, moq-transport-16, moq-transport-17
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[arg(id = "client-version", long = "client-version", env = "MOQ_CLIENT_VERSION")]
	pub version: Vec<moq_net::Version>,

	/// TLS trust and client-certificate settings (`--client-tls-*`).
	#[command(flatten)]
	#[serde(default)]
	pub tls: crate::tls::Client,

	/// Retry pacing for [`Client::reconnect`] (`--client-backoff-*`).
	#[command(flatten)]
	#[serde(default)]
	pub backoff: Backoff,

	/// WebSocket fallback settings (`--client-websocket-*`), used when QUIC is
	/// blocked.
	#[cfg(feature = "websocket")]
	#[command(flatten)]
	#[serde(default)]
	pub websocket: crate::websocket::Client,
}

impl ClientConfig {
	/// Build the [`Client`] this config describes.
	pub fn init(self) -> crate::Result<Client> {
		Client::new(self)
	}

	/// Returns the configured versions, defaulting to all if none specified.
	pub fn versions(&self) -> moq_net::Versions {
		if self.version.is_empty() {
			moq_net::Versions::all()
		} else {
			moq_net::Versions::from(self.version.clone())
		}
	}

	/// The Happy Eyeballs stagger to use, resolving the default. Every backend
	/// reads it from here so the four dial paths can't drift apart.
	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche", feature = "tcp"))]
	pub(crate) fn effective_failover_delay(&self) -> std::time::Duration {
		self.failover_delay.unwrap_or(crate::failover::DEFAULT_DELAY)
	}

	fn connect_timeout(&self) -> std::time::Duration {
		self.timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT)
	}
}

impl Default for ClientConfig {
	fn default() -> Self {
		Self {
			connect: None,
			bind: "[::]:0".parse().unwrap(),
			backend: None,
			failover_delay: None,
			timeout: None,
			quic: crate::quic::Client::default(),
			version: Vec::new(),
			tls: crate::tls::Client::default(),
			backoff: Backoff::default(),
			#[cfg(feature = "websocket")]
			websocket: crate::websocket::Client::default(),
		}
	}
}

/// Client for establishing MoQ connections over QUIC, WebTransport, or WebSocket.
///
/// Create via [`ClientConfig::init`] or [`Client::new`].
#[derive(Clone)]
pub struct Client {
	moq: moq_net::Client,
	/// The single resolved set of protocol versions, used to advertise moq ALPNs across
	/// every transport (passed into the QUIC backends' `connect` and used directly for
	/// raw TCP/UDS qmux and WebSocket). Resolved once in [`Client::new`] so the ALPN list
	/// can't diverge between transports.
	versions: moq_net::Versions,
	/// The URL from [`ClientConfig::connect`], dialed by [`Client::publish`] / [`Client::consume`].
	connect: Option<Url>,
	/// Deadline for one [`Client::connect`], from [`ClientConfig::timeout`]. Zero waits forever.
	timeout: std::time::Duration,
	backoff: Backoff,
	/// The resolved Happy Eyeballs stagger, used by the `tcp://` dial here; the
	/// QUIC backends capture their own copy from the config.
	#[cfg(feature = "tcp")]
	failover_delay: std::time::Duration,
	#[cfg(feature = "websocket")]
	websocket: crate::websocket::Client,
	tls: rustls::ClientConfig,
	#[cfg(feature = "noq")]
	noq: Option<crate::noq::NoqClient>,
	#[cfg(feature = "quinn")]
	quinn: Option<crate::quinn::QuinnClient>,
	#[cfg(feature = "quiche")]
	quiche: Option<crate::quiche::QuicheClient>,
	#[cfg(feature = "iroh")]
	iroh: Option<crate::iroh::Endpoint>,
	#[cfg(feature = "iroh")]
	iroh_addrs: Vec<std::net::SocketAddr>,
}

impl Client {
	/// Build a client from its config.
	///
	/// Errors if no transport feature is compiled in.
	#[cfg(not(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	)))]
	pub fn new(_config: ClientConfig) -> crate::Result<Self> {
		Err(Error::NoBackend(
			"no QUIC or WebSocket backend compiled; enable noq, quinn, quiche, websocket, tcp, or uds feature",
		))
	}

	/// Build a client from its config, binding the QUIC socket up front.
	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	pub fn new(config: ClientConfig) -> crate::Result<Self> {
		#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
		let backend = config.backend.clone().unwrap_or_else(crate::default_quic_backend);

		config.quic.validate()?;

		let tls = config.tls.build()?;

		#[cfg(feature = "noq")]
		#[allow(unreachable_patterns)]
		let noq = match backend {
			QuicBackend::Noq => Some(crate::noq::NoqClient::new(&config)?),
			_ => None,
		};

		#[cfg(feature = "quinn")]
		#[allow(unreachable_patterns)]
		let quinn = match backend {
			QuicBackend::Quinn => Some(crate::quinn::QuinnClient::new(&config)?),
			_ => None,
		};

		#[cfg(feature = "quiche")]
		let quiche = match backend {
			QuicBackend::Quiche => Some(crate::quiche::QuicheClient::new(&config)?),
			_ => None,
		};

		let versions = config.versions();
		// Read before the struct literal below moves fields out of `config`.
		#[cfg(feature = "tcp")]
		let failover_delay = config.effective_failover_delay();
		let timeout = config.connect_timeout();

		Ok(Self {
			moq: moq_net::Client::new().with_versions(versions.clone()),
			versions,
			connect: config.connect,
			timeout,
			backoff: config.backoff,
			#[cfg(feature = "tcp")]
			failover_delay,
			#[cfg(feature = "websocket")]
			websocket: config.websocket,
			tls,
			#[cfg(feature = "noq")]
			noq,
			#[cfg(feature = "quinn")]
			quinn,
			#[cfg(feature = "quiche")]
			quiche,
			#[cfg(feature = "iroh")]
			iroh: None,
			#[cfg(feature = "iroh")]
			iroh_addrs: Vec::new(),
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

	/// Subscribe to the peer's broadcasts, ingesting them into the given origin.
	pub fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		self.moq = self.moq.with_subscriber(subscribe);
		self
	}

	/// Attach a per-connection [`moq_net::stats::Session`] context to all sessions
	/// opened by this client.
	pub fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.moq = self.moq.with_stats(stats);
		self
	}

	/// Price the links this client dials; see [`moq_net::Client::with_cost`].
	pub fn with_cost(mut self, cost: u64) -> Self {
		self.moq = self.moq.with_cost(cost);
		self
	}

	/// Assign an origin (hop) id to the peers this client dials, used whenever a
	/// peer doesn't declare one itself; see [`moq_net::Client::with_peer_origin`].
	pub fn with_peer_origin(mut self, origin: moq_net::Origin) -> Self {
		self.moq = self.moq.with_peer_origin(origin);
		self
	}

	/// Start a background reconnect loop that connects to the given URL,
	/// waits for the session to close, then reconnects with exponential backoff.
	///
	/// Returns a [`Reconnect`] handle; drop the last handle to stop the loop.
	pub fn reconnect(&self, url: Url) -> Reconnect {
		Reconnect::new(self.clone(), url, self.backoff.clone())
	}

	/// Dial the configured [`ClientConfig::connect`] URL, publishing `origin` to it
	/// and reconnecting with backoff until the returned handle is dropped.
	///
	/// Returns `None` when no `--client-connect` URL was configured, so a caller
	/// that may run server-only doesn't have to branch on the URL itself.
	pub fn publish(self, origin: moq_net::origin::Consumer) -> Option<Reconnect> {
		let url = self.connect.clone()?;
		Some(self.with_publisher(origin).reconnect(url))
	}

	/// Dial the configured [`ClientConfig::connect`] URL, consuming its broadcasts
	/// into `origin` and reconnecting with backoff until the returned handle is
	/// dropped.
	///
	/// Broadcasts fed by these sessions linger across a session drop for as long
	/// as the reconnect loop keeps retrying ([`Backoff::linger`]): a relay restart
	/// is a bounded gap the reconnect splices over, not a teardown. When the loop
	/// gives up, its error surfaces (via [`Reconnect::closed`]) just before the
	/// broadcasts abort.
	///
	/// Returns `None` when no `--client-connect` URL was configured.
	pub fn consume(self, origin: moq_net::origin::Producer) -> Option<Reconnect> {
		let url = self.connect.clone()?;
		let origin = origin.with_linger(self.backoff.linger());
		Some(self.with_subscriber(origin).reconnect(url))
	}

	/// Dial the given URL and complete the MoQ handshake.
	///
	/// Errors if no transport feature is compiled in.
	#[cfg(not(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	)))]
	pub async fn connect(&self, _url: Url) -> crate::Result<moq_net::Session> {
		Err(Error::NoBackend(
			"no backend compiled; enable noq, quinn, quiche, iroh, websocket, tcp, or uds feature",
		))
	}

	/// Dial the given URL and complete the MoQ handshake.
	///
	/// The scheme picks the transport, and `https://` races QUIC against the
	/// WebSocket fallback so a blocked UDP path still connects. The session's
	/// protocol driver is spawned on the current tokio runtime; the session
	/// closes once the last returned handle drops.
	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	pub async fn connect(&self, url: Url) -> crate::Result<moq_net::Session> {
		// Each compiled backend adds state to this dispatch future. Keep it off the
		// caller's stack so all-feature builds remain safe on standard 2 MiB threads.
		let attempt = Box::pin(self.connect_inner(url));

		// The deadline covers the dial AND the handshake, for every transport: it is the
		// only bound some of them have. Dropping `attempt` on expiry cancels whichever
		// arm was still pending.
		let pair = match self.timeout.is_zero() {
			true => attempt.await?,
			false => match tokio::time::timeout(self.timeout, attempt).await {
				Ok(res) => res?,
				Err(_) => return Err(Error::ConnectTimeout(self.timeout)),
			},
		};

		tracing::info!(version = %pair.0.version(), "connected");
		Ok(crate::spawn_session(pair))
	}

	/// The moq client builder, advertising `path` in the SETUP when there is one.
	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	fn moq_with_path(&self, path: Option<String>) -> moq_net::Client {
		match path {
			Some(path) => self.moq.clone().with_path(path),
			None => self.moq.clone(),
		}
	}

	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	async fn connect_inner(&self, url: Url) -> crate::Result<(moq_net::Session, moq_net::Driver)> {
		// Transports with no request URI of their own advertise the request target in the
		// SETUP instead; `setup_path` returns `None` for the ones that carry a URI, where
		// sending it again is a protocol violation.
		let moq = self.moq_with_path(setup_path(&url));

		// Plain TCP (qmux, no TLS). Explicit opt-in scheme; never raced against
		// QUIC, which can't speak it. Use only on a trusted network.
		#[cfg(feature = "tcp")]
		if url.scheme() == "tcp" {
			let session = crate::tcp::connect(url, &self.versions.alpns(), self.failover_delay).await?;
			return Ok(moq.connect(session).await?);
		}

		// Unix domain socket (qmux, no TLS). Same-host only; the server can
		// authenticate us by uid/gid via SO_PEERCRED.
		#[cfg(all(feature = "uds", unix))]
		if url.scheme() == "unix" {
			let session = crate::unix::connect(url, &self.versions.alpns()).await?;
			return Ok(moq.connect(session).await?);
		}

		// iroh offers the moq ALPNs ahead of H3, so two moq endpoints normally land on raw
		// QUIC, which carries no request URI. The scheme can't tell us which we got, so the
		// request target waits on the negotiated binding: the SETUP for raw QUIC, the
		// CONNECT URL for H3 (where a SETUP path would be a protocol violation).
		#[cfg(feature = "iroh")]
		if url.scheme() == "iroh" {
			let endpoint = self.iroh.as_ref().ok_or(Error::IrohDisabled)?;
			let target = request_target(&url);
			let (session, binding) = crate::iroh::connect(endpoint, url, self.iroh_addrs.iter().copied()).await?;

			let moq = match binding {
				crate::iroh::Binding::Raw => self.moq_with_path(target),
				crate::iroh::Binding::H3 => self.moq.clone(),
			};

			return Ok(moq.connect(session).await?);
		}

		#[cfg(feature = "noq")]
		if let Some(noq) = self.noq.as_ref() {
			let tls = self.tls.clone();
			let quic_url = url.clone();
			let quic_handle = async { noq.connect(&tls, quic_url, &self.versions).await.map_err(Error::from) };

			#[cfg(feature = "websocket")]
			{
				return self.race_moq_connect(&moq, url, quic_handle).await;
			}

			#[cfg(not(feature = "websocket"))]
			{
				let session = quic_handle.await?;
				return Ok(moq.connect(session).await?);
			}
		}

		#[cfg(feature = "quinn")]
		if let Some(quinn) = self.quinn.as_ref() {
			let tls = self.tls.clone();
			let quic_url = url.clone();
			let quic_handle = async { quinn.connect(&tls, quic_url, &self.versions).await.map_err(Error::from) };

			#[cfg(feature = "websocket")]
			{
				return self.race_moq_connect(&moq, url, quic_handle).await;
			}

			#[cfg(not(feature = "websocket"))]
			{
				let session = quic_handle.await?;
				return Ok(moq.connect(session).await?);
			}
		}

		#[cfg(feature = "quiche")]
		if let Some(quiche) = self.quiche.as_ref() {
			let quic_url = url.clone();
			let quic_handle = async { quiche.connect(quic_url, &self.versions).await.map_err(Error::from) };

			#[cfg(feature = "websocket")]
			{
				return self.race_moq_connect(&moq, url, quic_handle).await;
			}

			#[cfg(not(feature = "websocket"))]
			{
				let session = quic_handle.await?;
				return Ok(moq.connect(session).await?);
			}
		}

		#[cfg(feature = "websocket")]
		{
			let alpns = self.versions.alpns();
			let session = crate::websocket::connect(&self.websocket, &self.tls, url, &alpns).await?;
			return Ok(moq.connect(session).await?);
		}

		#[cfg(not(feature = "websocket"))]
		return Err(Error::NoBackend("no QUIC backend matched; this should not happen"));
	}

	/// Race the QUIC dial against the WebSocket fallback, handshaking whichever wins.
	///
	/// `moq` is the QUIC-side builder, which carries the SETUP path for a raw QUIC dial.
	/// The WebSocket fallback uses the plain builder: qmux over WebSocket carries the
	/// path in its request URI, so repeating it in the SETUP is a protocol violation.
	#[cfg(feature = "websocket")]
	async fn race_moq_connect<Q, S>(
		&self,
		moq: &moq_net::Client,
		url: Url,
		quic: Q,
	) -> crate::Result<(moq_net::Session, moq_net::Driver)>
	where
		Q: Future<Output = crate::Result<S>>,
		S: web_transport_trait::Session,
	{
		let alpns = self.versions.alpns();
		let ws_config = self.websocket.clone();
		let ws_tls = self.tls.clone();
		let websocket = async move {
			crate::websocket::race_handle(&ws_config, &ws_tls, url, &alpns)
				.await
				.map(|res| res.map_err(Error::from))
		};

		match race_transport_connect(quic, websocket).await? {
			TransportRace::Quic(quic) => Ok(moq.connect(quic).await?),
			TransportRace::WebSocket(websocket) => Ok(self.moq.connect(websocket).await?),
		}
	}
}

/// The request target a URI-less transport advertises in its SETUP: the URL path, plus
/// `?` and the query when there is one (draft-ietf-moq-transport-19, section 10.3.1.2).
/// That query is how `?jwt=` reaches a relay.
///
/// `None` when the result is empty, which means the same as omitting the parameter: the
/// server's default path. A peer on published lite-05 rejects an empty value outright.
#[cfg(any(
	feature = "noq",
	feature = "quinn",
	feature = "quiche",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	feature = "uds"
))]
fn request_target(url: &Url) -> Option<String> {
	// A trailing `?` parses as an empty query, which is not a query: appending it would
	// spell one target two ways, and `moqt://host?` would yield a bare "?" rather than
	// the empty value that means the default path.
	let target = match url.query().filter(|query| !query.is_empty()) {
		Some(query) => format!("{}?{}", url.path(), query),
		None => url.path().to_owned(),
	};

	(!target.is_empty()).then_some(target)
}

/// The request target to advertise in the SETUP, chosen by the dial URL's scheme.
///
/// `None` for the schemes whose transport carries a request URI of its own
/// (WebTransport, qmux over WebSocket): they convey the target there, and a SETUP path
/// on top of it is a protocol violation. `iroh` is `None` here because its binding is
/// picked by ALPN negotiation rather than by the scheme; that dial reads the negotiated
/// [`crate::iroh::Binding`] and calls [`request_target`] itself.
#[cfg(any(
	feature = "noq",
	feature = "quinn",
	feature = "quiche",
	feature = "iroh",
	feature = "websocket",
	feature = "tcp",
	feature = "uds"
))]
fn setup_path(url: &Url) -> Option<String> {
	match url.scheme() {
		// A Unix socket URL's path is the socket file, so the request target rides in
		// the `?path=` query, query string and all. It is one form-encoded value, so a
		// target that carries its own `?query` percent-encodes it.
		"unix" => url
			.query_pairs()
			.find(|(k, _)| k == "path")
			.map(|(_, v)| v.into_owned())
			.filter(|path| !path.is_empty()),
		// Raw QUIC and qmux over TCP negotiate an ALPN and nothing else, so the whole
		// request target travels in the SETUP.
		"moqt" | "moql" | "tcp" => request_target(url),
		_ => None,
	}
}

#[cfg(feature = "websocket")]
#[derive(Debug, PartialEq, Eq)]
enum TransportRace<Q, W> {
	Quic(Q),
	WebSocket(W),
}

#[cfg(feature = "websocket")]
async fn race_transport_connect<Q, W, QT, WT>(quic: Q, websocket: W) -> crate::Result<TransportRace<QT, WT>>
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
					Ok(session) => return Ok(TransportRace::Quic(session)),
					Err(err) if err.is_auth() => return Err(err),
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
					Some(Err(err)) if err.is_auth() => return Err(err),
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
		(Some(quic), Some(websocket)) => Err(Error::TransportRace {
			quic: std::sync::Arc::new(quic),
			websocket: std::sync::Arc::new(websocket),
		}),
		(Some(err), None) | (None, Some(err)) => Err(err),
		(None, None) => Err(Error::ConnectFailed),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	#[test]
	fn setup_path_covers_the_uri_less_transports() {
		// An empty path and an absent one both mean the server's default, so we send
		// neither. A peer on published lite-05 rejects an empty value outright.
		let cases = [
			("unix:///run/moq.sock?path=/room", Some("/room")),
			// The whole resource path is one form-encoded value, so a `?query` inside it
			// arrives percent-encoded and comes back out whole.
			("unix:///run/moq.sock?path=/room%3Fjwt%3Dabc", Some("/room?jwt=abc")),
			("unix:///run/moq.sock?path=", None),
			("unix:///run/moq.sock", None),
			("tcp://localhost:4443/room", Some("/room")),
			("tcp://localhost:4443/room?jwt=abc", Some("/room?jwt=abc")),
			("tcp://localhost:4443", None),
			// Raw QUIC: the URL is ours alone, so the path and query have to ride the
			// SETUP or the server never sees them.
			("moqt://relay.example.com/anon", Some("/anon")),
			("moqt://relay.example.com/anon?jwt=abc", Some("/anon?jwt=abc")),
			("moql://relay.example.com/anon?jwt=abc", Some("/anon?jwt=abc")),
			("moqt://relay.example.com", None),
			// The fragment is processed by the client and never sent (draft-19 3.1.2).
			("moqt://relay.example.com/anon?jwt=abc#pos:12", Some("/anon?jwt=abc")),
			("moqt://relay.example.com/anon#pos:12", Some("/anon")),
			// A trailing `?` is an empty query, not a query.
			("moqt://relay.example.com/anon?", Some("/anon")),
			("moqt://relay.example.com?", None),
			// The transport's own request URI carries the path, so sending one here
			// would be a protocol violation.
			("https://relay.example.com/anon?jwt=abc", None),
			("http://relay.example.com/anon", None),
			("wss://relay.example.com/anon?jwt=abc", None),
			// Decided after the ALPN is negotiated, not here.
			("iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/anon", None),
		];

		for (url, want) in cases {
			let url = Url::parse(url).unwrap();
			let got = setup_path(&url);
			assert_eq!(got.as_deref(), want, "{url}");
		}
	}

	/// The iroh dial derives its target here rather than through [`setup_path`], since
	/// only the negotiated binding says whether to send one.
	#[cfg(any(
		feature = "noq",
		feature = "quinn",
		feature = "quiche",
		feature = "iroh",
		feature = "websocket",
		feature = "tcp",
		feature = "uds"
	))]
	#[test]
	fn request_target_joins_the_path_and_query() {
		const PEER: &str = "k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa";

		let cases = [
			(format!("iroh://{PEER}/room?jwt=abc"), Some("/room?jwt=abc")),
			(format!("iroh://{PEER}/room"), Some("/room")),
			(format!("iroh://{PEER}"), None),
			(format!("iroh://{PEER}/"), Some("/")),
		];

		for (url, want) in cases {
			let url = Url::parse(&url).unwrap();
			let got = request_target(&url);
			assert_eq!(got.as_deref(), want, "{url}");
		}
	}

	#[test]
	fn test_toml_disable_verify_survives_update_from() {
		let toml = r#"
			tls.disable_verify = true
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.tls.disable_verify, Some(true));

		// Simulate: TOML loaded, then CLI args re-applied (no --client-tls-disable-verify flag).
		config.update_from(["test"]);
		assert_eq!(config.tls.disable_verify, Some(true));
	}

	#[test]
	fn test_cli_disable_verify_flag() {
		let config = ClientConfig::parse_from(["test", "--client-tls-disable-verify"]);
		assert_eq!(config.tls.disable_verify, Some(true));
	}

	#[test]
	fn test_cli_disable_verify_explicit_false() {
		let config = ClientConfig::parse_from(["test", "--client-tls-disable-verify=false"]);
		assert_eq!(config.tls.disable_verify, Some(false));
	}

	#[test]
	fn test_cli_disable_verify_explicit_true() {
		let config = ClientConfig::parse_from(["test", "--client-tls-disable-verify=true"]);
		assert_eq!(config.tls.disable_verify, Some(true));
	}

	#[test]
	fn test_cli_deprecated_tls_flags_fold_into_canonical() {
		// The bare --tls-* forms are deprecated. They parse into a hidden field and
		// fold into the canonical values via the effective_* accessors build() uses,
		// so they keep working without touching the public Client fields.
		let config = ClientConfig::parse_from(["test", "--tls-disable-verify=true", "--tls-fingerprint", "abcd1234"]);
		assert_eq!(
			config.tls.disable_verify, None,
			"deprecated flag must not set the canonical field"
		);
		assert_eq!(config.tls.effective_disable_verify(), Some(true));
		assert_eq!(config.tls.effective_fingerprint(), vec!["abcd1234"]);
	}

	#[test]
	fn test_canonical_tls_flag_wins_over_deprecated() {
		// Both spellings given: canonical wins for scalar options, vecs concatenate.
		let config = ClientConfig::parse_from([
			"test",
			"--client-tls-disable-verify=false",
			"--tls-disable-verify=true",
			"--client-tls-fingerprint",
			"aaaa",
			"--tls-fingerprint",
			"bbbb",
		]);
		assert_eq!(config.tls.effective_disable_verify(), Some(false));
		assert_eq!(config.tls.effective_fingerprint(), vec!["aaaa", "bbbb"]);
	}

	#[test]
	fn test_cli_no_disable_verify() {
		let config = ClientConfig::parse_from(["test"]);
		assert_eq!(config.tls.disable_verify, None);
	}

	#[test]
	fn test_toml_failover_delay_survives_update_from() {
		let toml = r#"
			failover_delay = "1s"
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.failover_delay, Some(std::time::Duration::from_secs(1)));

		// Simulate: TOML loaded, then CLI args re-applied (no --client-failover-delay flag).
		config.update_from(["test"]);
		assert_eq!(config.failover_delay, Some(std::time::Duration::from_secs(1)));
	}

	#[test]
	fn test_cli_failover_delay() {
		let config = ClientConfig::parse_from(["test", "--client-failover-delay", "50ms"]);
		assert_eq!(config.failover_delay, Some(std::time::Duration::from_millis(50)));
	}

	#[test]
	fn test_toml_fingerprint_survives_update_from() {
		let toml = r#"
			tls.fingerprint = ["abcd1234", "ef567890"]
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.tls.fingerprint, vec!["abcd1234", "ef567890"]);

		// Simulate: TOML loaded, then CLI args re-applied (no --client-tls-fingerprint flag).
		config.update_from(["test"]);
		assert_eq!(config.tls.fingerprint, vec!["abcd1234", "ef567890"]);
	}

	#[test]
	fn test_toml_fingerprint_accepts_single_string() {
		let toml = r#"
			tls.fingerprint = "abcd1234"
		"#;

		let config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.tls.fingerprint, vec!["abcd1234"]);
	}

	#[test]
	fn test_cli_fingerprint() {
		let config = ClientConfig::parse_from(["test", "--client-tls-fingerprint", "abcd1234"]);
		assert_eq!(config.tls.fingerprint, vec!["abcd1234"]);
	}

	#[test]
	fn test_toml_version_survives_update_from() {
		let toml = r#"
			version = ["moq-lite-02"]
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.version, vec!["moq-lite-02".parse::<moq_net::Version>().unwrap()]);

		// Simulate: TOML loaded, then CLI args re-applied (no --client-version flag).
		config.update_from(["test"]);
		assert_eq!(config.version, vec!["moq-lite-02".parse::<moq_net::Version>().unwrap()]);
	}

	#[test]
	fn test_cli_version() {
		let config = ClientConfig::parse_from(["test", "--client-version", "moq-lite-03"]);
		assert_eq!(config.version, vec!["moq-lite-03".parse::<moq_net::Version>().unwrap()]);
	}

	#[test]
	fn test_toml_connect_survives_update_from() {
		let toml = r#"
			connect = "https://relay.example.com/anon"
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(
			config.connect.as_ref().unwrap().as_str(),
			"https://relay.example.com/anon"
		);

		// Simulate: TOML loaded, then CLI args re-applied (no --client-connect flag).
		config.update_from(["test"]);
		assert_eq!(
			config.connect.as_ref().unwrap().as_str(),
			"https://relay.example.com/anon"
		);
	}

	#[test]
	fn test_cli_connect() {
		let config = ClientConfig::parse_from(["test", "--client-connect", "https://relay.example.com/anon"]);
		assert_eq!(
			config.connect.as_ref().unwrap().as_str(),
			"https://relay.example.com/anon"
		);
	}

	#[test]
	fn test_toml_host_name_survives_update_from() {
		let toml = r#"
			tls.host_name = "example.host"
		"#;

		let mut config: ClientConfig = toml::from_str(toml).unwrap();
		assert_eq!(config.tls.host_name.as_deref(), Some("example.host"));

		// Simulate: TOML loaded, then CLI args re-applied (no --client-tls-host-name flag).
		config.update_from(["test"]);
		assert_eq!(config.tls.host_name.as_deref(), Some("example.host"));
	}

	#[test]
	fn test_cli_host_name() {
		let config = ClientConfig::parse_from(["test", "--client-tls-host-name", "override.example"]);
		assert_eq!(config.tls.host_name.as_deref(), Some("override.example"));
	}

	#[test]
	fn test_cli_no_version_defaults_to_all() {
		let config = ClientConfig::parse_from(["test"]);
		assert!(config.version.is_empty());
		// versions() helper returns all when none specified
		assert_eq!(config.versions().alpns().len(), moq_net::ALPNS.len());
	}

	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn race_transport_connect_stops_on_quic_auth_error() {
		let quic = async { Err::<usize, _>(crate::ConnectError::Unauthorized.into()) };
		let websocket = async {
			// This only needs to complete later than the immediately ready QUIC auth error.
			tokio::task::yield_now().await;
			Some(Ok(1usize))
		};

		let err = super::race_transport_connect(quic, websocket).await.unwrap_err();
		assert_eq!(err.connect_error(), Some(crate::ConnectError::Unauthorized));
	}

	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn race_transport_connect_keeps_websocket_after_quic_non_auth_error() {
		let quic = async { Err::<usize, _>(Error::ConnectFailed) };
		let websocket = async { Some(Ok(7usize)) };

		let value = super::race_transport_connect(quic, websocket).await.unwrap();
		assert_eq!(value, super::TransportRace::WebSocket(7));
	}

	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn race_transport_connect_returns_when_quic_transport_connects() {
		let quic = async { Ok("quic") };
		let websocket = std::future::pending::<Option<crate::Result<&str>>>();

		let value = tokio::time::timeout(
			std::time::Duration::from_secs(1),
			super::race_transport_connect(quic, websocket),
		)
		.await
		.expect("race waited for WebSocket after QUIC transport connected")
		.unwrap();
		assert_eq!(value, super::TransportRace::Quic("quic"));
	}

	#[test]
	fn connect_timeout_defaults_to_thirty_seconds() {
		let config = ClientConfig::parse_from(["test"]);
		assert_eq!(config.timeout, None);
		assert_eq!(config.connect_timeout(), DEFAULT_CONNECT_TIMEOUT);
	}

	/// A peer that completes the TCP handshake and then never speaks: the QUIC arm
	/// gives up on its own, but the WebSocket arm has no deadline of its own, so the
	/// race stays pending forever. Without the connect timeout this test hangs.
	///
	/// That is what wedged a publisher against a livelocked relay: `Reconnect` only
	/// re-arms its backoff (and checks its give-up timeout) *between* attempts, so an
	/// attempt that never returns stalls the retry loop for good.
	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn connect_times_out_against_a_peer_that_never_speaks() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();

		let timeout = DEFAULT_CONNECT_TIMEOUT;
		let mut config = ClientConfig {
			timeout: Some(timeout),
			..Default::default()
		};
		config.websocket.delay = Some(std::time::Duration::ZERO);
		let client = config.init().unwrap();

		// Nothing is listening on UDP, so the QUIC arm fails and leaves the WebSocket
		// arm alone against the silent peer.
		let url: Url = format!("https://127.0.0.1:{}/", addr.port()).parse().unwrap();

		let mut attempt = Box::pin(client.connect(url));
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
