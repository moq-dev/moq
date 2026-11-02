//! Accepting a MoQ session, including the paused handshake that inspects the
//! peer's SETUP before granting origins.

use crate::driver::Transport;
use crate::origin;
use crate::time::{Deadlines, Instant};
use crate::transport::poll::Session as _;
use crate::{
	ALPN_18, ALPN_20, ALPN_22, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05, ALPN_LITE_06, ALPN_LITE_07_WIP,
	Consume, Error, NEGOTIATED, Session, SessionError, Version, Versions,
	coding::{Encode, Stream},
	ietf, lite,
	setup::{self, Role},
	stats,
};

/// Tracks admitted connections until their handshake or session ends.
#[derive(Clone, Debug)]
pub struct Activity {
	state: kio::Producer<()>,
	// Admission goes through the weak handle: its open check shares the lock
	// `close` commits under, so a connection is either counted or refused.
	admit: kio::ProducerWeak<()>,
}

impl Default for Activity {
	fn default() -> Self {
		let state = kio::Producer::default();
		let admit = state.weak();
		Self { state, admit }
	}
}

impl Activity {
	/// Hold the server active until the returned guard is dropped, or `None` once closed.
	pub fn enter(&self) -> Option<Active> {
		Some(Active {
			_guard: self.admit.try_consume()?,
		})
	}

	/// Wait until every admitted connection has left, then refuse new ones.
	pub async fn close(&self) {
		loop {
			if self.state.wanted_changed(true).await.is_err() {
				return;
			}
			match self.state.write_unwanted() {
				kio::Unwanted::Idle(state) => return state.close(),
				// Admitted between the wake and the commit: wait for it too.
				kio::Unwanted::Wanted => continue,
				kio::Unwanted::Closed => return,
			}
		}
	}
}

/// Keeps an admitted connection in the server's active set.
#[derive(Debug)]
#[must_use]
pub struct Active {
	_guard: kio::Consumer<()>,
}

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Session,
	versions: Versions,
	limits: crate::session::Limits,
	extensions: setup::Extensions,
	implementation: Option<String>,
}

impl Server {
	/// A server that neither publishes nor subscribes until configured.
	pub fn new() -> Self {
		Default::default()
	}

	/// Publish to the connected client: the session reads from the given origin
	/// (pass an [`origin::Producer`] or [`origin::Consumer`] by reference) and forwards
	/// its announcements. Omit to publish nothing. Pre-scoped via
	/// [`origin::Producer::scope`] for token-gated relays.
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client: the session writes the broadcasts the
	/// client announces into this [`origin::Producer`]. Omit to subscribe to nothing.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.subscribe = Some(subscribe);
		self
	}

	/// Attach a per-connection [`stats::Session`] context. The session's publish
	/// (egress) and subscribe (ingress) origin handles are tagged with it, so all
	/// traffic counters are attributed through the model for this session's lifetime,
	/// and how the session ended is recorded there.
	/// Pass [`stats::Session::default`] (a no-op context) to opt out.
	pub fn with_stats(mut self, stats: stats::Session) -> Self {
		self.stats = stats;
		self
	}

	/// Cap what each client can make its session hold. Defaults to [`session::Limits::default`](crate::session::Limits::default).
	pub fn with_limits(mut self, limits: crate::session::Limits) -> Self {
		self.limits = limits;
		self
	}

	/// Restrict which protocol versions to accept, in preference order.
	/// Defaults to every version this crate supports.
	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Choose which extensions to offer in SETUP. Defaults to every moq-transport one, and
	/// not [`ietf`](setup::Extensions::ietf), which only relays turn on.
	pub fn with_extensions(mut self, extensions: setup::Extensions) -> Self {
		self.extensions = extensions;
		self
	}

	/// Set the implementation to advertise in SETUP, as space-separated `name/version`
	/// product tokens, outermost first. Defaults to [`IMPLEMENTATION`](crate::IMPLEMENTATION);
	/// an empty string advertises nothing.
	pub fn with_implementation(mut self, implementation: impl Into<String>) -> Self {
		self.implementation = Some(implementation.into());
		self
	}

	/// The Implementation our SETUP carries, or `None` to omit it.
	fn implementation(&self) -> Option<String> {
		setup::implementation(self.implementation.as_deref().unwrap_or(crate::IMPLEMENTATION))
	}

	/// The configured origin pair, each tagged with the stats context so the
	/// model attributes reads (egress) and writes (ingress) for this session.
	/// One shared context across both halves keeps presence and viewer counts
	/// from double-attributing.
	fn stat_tagged_origins(&self) -> (Option<origin::Consumer>, Option<origin::Producer>) {
		let publish = self.publish.clone().map(|origin| origin.with_stats(self.stats.clone()));
		let subscribe = self
			.subscribe
			.clone()
			.map(|origin| origin.with_stats(self.stats.clone()));
		(publish, subscribe)
	}

	/// Start a lite session on an accepted transport: wire the origins, answer
	/// with our SETUP, and return the session and its driver.
	fn start_lite(
		&self,
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
		client_setup: Option<lite::AcceptedSetup>,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Link,
	) -> Result<(Session, crate::Driver), Error> {
		let (publish, subscribe) = self.stat_tagged_origins();

		// We report what the transport actually measures; a server never
		// advertises a request Path or Role, and only the dialing side prices a
		// link. Versions without a Setup Stream have nothing to advertise.
		let our_setup = if version.has_setup_stream() {
			lite::Setup {
				probe: lite::ProbeLevel::detect(&session),
				path: None,
				role: None,
				cost: None,
				// Filled by `lite::start` from the attached origin handles.
				hop: None,
				token: None,
				ietf: self.extensions.ietf && version.has_ietf(),
				implementation: self.implementation(),
			}
		} else {
			lite::Setup::default()
		};

		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			stats: self.stats.clone(),
			client: false,
			limits: self.limits,
			session: session.clone(),
			setup_stream: None,
			publish,
			subscribe,
			peer_hop,
			version,
			our_setup,
			peer_setup: client_setup,
			auth,
		})?;

		Ok(Session::new(
			runtime,
			session,
			version.into(),
			start.recv_bandwidth,
			crate::driver::Protocol::Lite(Box::new(start.driver)),
			crate::session::Handles {
				stats: self.stats.clone(),
				goaway: start.goaway,
				auth: start.auth,
				credential: start.credential,
				estimate: Some(start.estimate),
				bandwidth: crate::bandwidth::Allocator::default(),
				peer_implementation: start.peer_implementation,
			},
		))
	}

	async fn handshake_lite(
		&self,
		runtime: Deadlines,
		mut session: Transport,
		version: lite::Version,
	) -> Result<Handshake, Error> {
		self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
		let (path, role, origin, peer_implementation, handshake) = if version.has_setup_stream() {
			// Gate on the client's SETUP: read it before serving so the
			// caller can scope by the advertised path. Seeded back into
			// `start` on `ok()` so PROBE gating resolves without
			// re-reading the (consumed) Setup Stream.
			let client_setup = lite::accept_setup(&mut session, version).await?;
			(
				client_setup.setup.path.clone(),
				client_setup.setup.role,
				client_setup.setup.hop,
				client_setup.setup.implementation.clone(),
				PausedHandshake::LiteSetup {
					session,
					version,
					client_setup,
				},
			)
		} else {
			(None, None, None, None, PausedHandshake::LiteBare { session, version })
		};

		let auth = crate::auth::Link::new(match &handshake {
			PausedHandshake::LiteBare { version, .. } | PausedHandshake::LiteSetup { version, .. } => {
				version.has_auth()
			}
			PausedHandshake::Ietf(_) | PausedHandshake::Negotiated(_) => false,
		});
		let token = match &handshake {
			PausedHandshake::LiteSetup { client_setup, .. } => client_setup.setup.token.clone(),
			_ => None,
		};
		Ok(Handshake {
			path,
			role,
			origin,
			token,
			peer_implementation,
			assigned_hop: crate::Hop::random(),
			auth,
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				deadline: None,
				handshake,
			}),
		})
	}

	/// Perform the MoQ handshake as a server, returning the [`Session`] and its [`Driver`](crate::Driver).
	///
	/// Poll the returned driver with nondecreasing time, starting at `now`.
	///
	/// Convenience wrapper over [`accept_request`](Self::accept_request) that
	/// completes the handshake immediately. Use `accept_request` when you need to
	/// inspect the client's advertised path before deciding what to serve.
	pub async fn accept(
		&self,
		now: Instant,
		session: impl crate::transport::poll::Boxable,
	) -> Result<(Session, crate::Driver), Error> {
		self.accept_request(now, session).await?.ok().await
	}

	/// Begin the MoQ handshake, pausing once the client's request path is known so
	/// the caller can authorize/scope before serving.
	///
	/// Reads the client's SETUP (the in-band path lives there on URL-less transports),
	/// then returns a [`Handshake`]: inspect [`path`](Handshake::path), set the origins to
	/// serve, and call [`ok`](Handshake::ok) or [`close`](Handshake::close). Session start
	/// is deferred to `ok()`, so origins set on the handshake always take effect.
	///
	/// The path is surfaced for moq-lite-05 and newer, and every moq-transport
	/// draft we speak; it's empty on versions with no in-band request path (lite 01-04).
	///
	/// A SETUP that fails to parse or negotiate closes the session with the matching code,
	/// so the peer learns why instead of seeing a bare disconnect.
	pub fn accept_request(
		&self,
		now: Instant,
		session: impl crate::transport::poll::Boxable,
	) -> impl Future<Output = Result<Handshake, Error>> {
		// Box before any protocol code runs, so it is compiled once rather than per transport.
		self.accept_boxed(Deadlines::new(now), Transport::new(session))
	}

	async fn accept_boxed(&self, runtime: Deadlines, session: Transport) -> Result<Handshake, Error> {
		let mut refused = session.clone();
		self.handshake(runtime, session)
			.await
			.inspect_err(|err| close(&mut refused, err, None))
	}

	async fn handshake(&self, runtime: Deadlines, mut session: Transport) -> Result<Handshake, Error> {
		let supported = match session.protocol() {
			Some(alpn @ (ALPN_22 | ALPN_20 | ALPN_18)) => {
				let draft = match alpn {
					ALPN_22 => ietf::Version::Draft22,
					ALPN_20 => ietf::Version::Draft20,
					_ => ietf::Version::Draft18,
				};

				self.versions.select(Version::Ietf(draft)).ok_or(Error::Version)?;
				return self.accept_ietf(runtime, session, draft).await;
			}
			Some(alpn @ (ALPN_LITE_07_WIP | ALPN_LITE_06 | ALPN_LITE_05 | ALPN_LITE_04 | ALPN_LITE_03)) => {
				let version = match alpn {
					ALPN_LITE_07_WIP => lite::Version::Lite07,
					ALPN_LITE_06 => lite::Version::Lite06,
					ALPN_LITE_05 => lite::Version::Lite05,
					ALPN_LITE_04 => lite::Version::Lite04,
					_ => lite::Version::Lite03,
				};
				return self.handshake_lite(runtime, session, version).await;
			}
			// No ALPN, or `moql`: negotiate moq-lite in a bidi SETUP.
			Some(ALPN_LITE) | None => self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?,
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		// Read the client's SETUP to choose the version; `ok()` sends the server SETUP and starts.
		// Stats attach in `ok()`: a caller may still replace them on the handshake.
		// The SETUP keeps lite-01's framing whichever version it offers.
		let mut stream = Stream::accept(&mut session, Version::Lite(lite::Version::Lite01)).await?;
		let (client, client_setup_size) = stream.reader.decode_sized::<setup::Client>().await?;

		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;
		let Version::Lite(version) = version else {
			return Err(Error::Version);
		};

		// Answer, close the stream, and continue as that version's ALPN would. Waiting for the
		// acknowledgement keeps the reply from being discarded by the reset a dropped stream sends.
		if Version::Lite(version).hands_off() {
			let Stream { mut writer, .. } = stream;
			writer
				.encode(&setup::Server {
					version: Version::Lite(version).into(),
					parameters: lite::Parameters::default().encode_bytes(version)?,
				})
				.await?;
			writer.close().await?;
			return self.handshake_lite(runtime, session, version).await;
		}

		Ok(Handshake {
			path: None,
			role: None,
			origin: None,
			token: None,
			peer_implementation: None,
			assigned_hop: crate::Hop::random(),
			// Lite 01/02 carry no AUTH.
			auth: crate::auth::Link::new(false),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				deadline: None,
				handshake: PausedHandshake::Negotiated(PausedNegotiated {
					session,
					stream,
					version,
					client_setup_size,
				}),
			}),
		})
	}

	/// Read a moq-transport client's SETUP (with its request path) off its uni stream,
	/// then pause. `ok()` starts the session and hands the stream back for GOAWAY.
	async fn accept_ietf(
		&self,
		runtime: Deadlines,
		mut session: Transport,
		version: ietf::Version,
	) -> Result<Handshake, Error> {
		let peer_setup = ietf::accept_setup(&mut session, version).await?;
		let options = &peer_setup.options;
		Ok(Handshake {
			path: options.path.clone(),
			role: None,
			// A moq-transport peer only has an identity if it negotiated the MoQ
			// Cluster extension and declared a non-zero Hop ID.
			origin: options.declared.cluster.hop.filter(|h| *h != crate::Hop::UNKNOWN),
			token: options.token.clone(),
			peer_implementation: options.implementation.clone(),
			assigned_hop: crate::Hop::random(),
			// The client's SETUP already settled whether it offered MoQ Auth, so it is
			// negotiated exactly when we offer it too.
			auth: crate::auth::Link::ietf(options.declared.auth && self.extensions.auth),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				deadline: None,
				handshake: PausedHandshake::Ietf(PausedIetf {
					session,
					version,
					peer_setup,
				}),
			}),
		})
	}
}

/// A paused server-side handshake.
///
/// Returned by [`Server::accept_request`] once the peer's advertised
/// [`path`](Self::path) is known but before the session is granted anything. Set
/// the origins to serve, then call [`ok`](Self::ok) to complete the handshake, or
/// [`close`](Self::close) to reject it. Modeled on the WebTransport `Request` in
/// moq-tokio.
pub struct Handshake {
	path: Option<String>,
	role: Option<Role>,
	origin: Option<crate::Hop>,
	token: Option<setup::Token>,
	peer_implementation: Option<String>,
	/// The identity this session's routes are attributed to for split horizon when the
	/// peer declares none on the wire; it never enters a hop chain. Fresh per request unless the caller overrides it
	/// ([`Handshake::with_peer_hop`]).
	assigned_hop: crate::Hop,
	/// The session's auth handle, available before [`Handshake::ok`] so the caller can
	/// take the peer's token requests before any arrive.
	auth: crate::auth::Link,
	// Taken by `ok`/`close`; `Drop` rejects the handshake if neither ran.
	inner: Option<RequestInner>,
}

/// The parts of a [`Handshake`] consumed by [`Handshake::ok`] / [`Handshake::close`].
struct RequestInner {
	server: Server,
	/// Supplies the clock and timers for the accepted session.
	runtime: Deadlines,
	/// When [`Handshake::ok`] gives up on a peer that has not taken the SETUP reply.
	deadline: Option<Instant>,
	handshake: PausedHandshake,
}

/// The handshake state captured at the pause point. Every variant defers its
/// session start to [`Handshake::ok`] so origins set on the handshake still apply.
enum PausedHandshake {
	/// moq-lite 03/04: no Setup Stream.
	LiteBare {
		session: Transport,
		version: lite::Version,
	},
	/// moq-lite 05+: the client's Setup Stream has been read. `ok()` starts the
	/// session, seeding the SETUP back so PROBE gating resolves.
	LiteSetup {
		session: Transport,
		version: lite::Version,
		client_setup: lite::AcceptedSetup,
	},
	Ietf(PausedIetf),
	Negotiated(PausedNegotiated),
}

/// moq-transport: the client's SETUP (with its request path) has been
/// read off its uni stream; `ok` starts the session, handing that stream back
/// for GOAWAY monitoring.
struct PausedIetf {
	session: Transport,
	version: ietf::Version,
	peer_setup: ietf::PeerSetup,
}

impl PausedIetf {
	/// Complete the handshake with the final server config.
	fn ok(
		self,
		server: Server,
		runtime: Deadlines,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Link,
	) -> Result<(Session, crate::Driver), Error> {
		let Self {
			session,
			version,
			peer_setup,
		} = self;
		server.stats.control(stats::Message::Session, peer_setup.size, true);
		let (publish, subscribe) = server.stat_tagged_origins();
		let peer_implementation = setup::PeerImplementation::known(peer_setup.options.implementation);

		// The client's SETUP was read at the pause; hand the stream back
		// for GOAWAY. A server never advertises a path, hence `None`.
		let (protocol, goaway, credential) = ietf::start(ietf::Config {
			runtime: runtime.clone(),
			stats: server.stats.clone(),
			limits: server.limits,
			session: session.clone(),

			client: false,
			publish,
			subscribe,
			peer_hop,
			// Only the dialing side prices a link.
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: Some(peer_setup.stream),
			peer_declared: Some(peer_setup.options.declared),
			auth: auth.clone(),
			early_unis: peer_setup.early,
			extensions: server.extensions,
			implementation: server.implementation(),
			peer_implementation: peer_implementation.clone(),
		})?;
		tracing::debug!(?version, "connected");
		Ok(Session::new(
			runtime,
			session,
			version.into(),
			None,
			crate::driver::Protocol::Ietf(protocol),
			crate::session::Handles {
				stats: server.stats.clone(),
				goaway,
				auth,
				credential,
				estimate: None,
				bandwidth: crate::bandwidth::Allocator::default(),
				peer_implementation,
			},
		))
	}
}

/// moq-lite 01/02: the client SETUP has been read off the bidi stream but the server
/// SETUP hasn't been sent; `ok` finishes it.
struct PausedNegotiated {
	session: Transport,
	stream: Stream<Transport, Version>,
	version: lite::Version,
	/// The client SETUP's encoded size, counted under the final stats.
	client_setup_size: usize,
}

impl PausedNegotiated {
	/// Send the server SETUP and start the session.
	async fn ok(
		self,
		server: Server,
		runtime: Deadlines,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Link,
	) -> Result<(Session, crate::Driver), Error> {
		let Self {
			session,
			stream,
			version,
			client_setup_size,
		} = self;
		let mut stream = stream.with_stats(&server.stats);
		server.stats.control(stats::Message::Session, client_setup_size, true);
		let (publish, subscribe) = server.stat_tagged_origins();

		let server_setup = setup::Server {
			version: Version::Lite(version).into(),
			parameters: lite::Parameters::default().encode_bytes(version)?,
		};
		stream.writer.encode(&server_setup).await?;

		let stream = stream.with_version(version);
		// Pre-lite-05: no Setup Stream, so nothing to advertise or seed.
		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			stats: server.stats.clone(),
			client: false,
			limits: server.limits,
			session: session.clone(),
			setup_stream: Some(stream),
			publish,
			subscribe,
			peer_hop,
			version,
			our_setup: lite::Setup::default(),
			peer_setup: None,
			auth,
		})?;

		Ok(Session::new(
			runtime,
			session,
			version.into(),
			start.recv_bandwidth,
			crate::driver::Protocol::Lite(Box::new(start.driver)),
			crate::session::Handles {
				stats: server.stats.clone(),
				goaway: start.goaway,
				auth: start.auth,
				credential: start.credential,
				estimate: Some(start.estimate),
				bandwidth: crate::bandwidth::Allocator::default(),
				peer_implementation: start.peer_implementation,
			},
		))
	}
}

impl Handshake {
	/// The request path the client advertised in its SETUP.
	///
	/// Empty when the client advertised none: either it sent an empty path, or the
	/// version carries none in-band (lite 01-04). Those mean the same thing, so the
	/// wire distinction isn't surfaced. Populated for moq-lite-05 and newer,
	/// and every moq-transport draft we speak. See the note on [`Server::accept_request`].
	pub fn path(&self) -> &str {
		self.path.as_deref().unwrap_or("")
	}

	/// The single [`Role`] the client advertised in its SETUP, or `None` for a
	/// bidirectional session.
	///
	/// Only moq-lite-05 and newer carry a role, so `None` covers three cases
	/// that the wire doesn't distinguish: an older version, a client that omitted the parameter, and a
	/// client that explicitly advertised both directions. All three mean the same thing
	/// (the client may publish and subscribe), so authorize on what the token grants.
	/// See the note on [`Server::accept_request`].
	pub fn role(&self) -> Option<Role> {
		self.role
	}

	/// The Hop ID declared by the peer, when the negotiated protocol carries one.
	///
	/// A moq-lite-05+ endpoint declares this when it attaches a publish or subscribe
	/// origin; a moq-transport endpoint declares it via the MoQ Cluster extension. Older
	/// versions and endpoints without one return `None`.
	///
	/// Self-declared, so treat it as a correlation hint rather than an
	/// authenticated identity: authorize on the token or client certificate.
	pub fn peer_hop(&self) -> Option<crate::Hop> {
		self.origin
	}

	/// The implementation the client advertised in its SETUP, such as `moq-net-rs/0.3.10`.
	///
	/// `None` when it advertised none or its version carries no SETUP (moq-lite 01-04),
	/// which in practice means it predates the parameter. Peer-controlled: use it for
	/// logs and rollout counts, never to authorize.
	pub fn peer_implementation(&self) -> Option<&str> {
		self.peer_implementation.as_deref()
	}

	/// The session's auth handle, the same one [`Session::auth`] returns once accepted.
	pub fn auth(&self) -> crate::auth::Handle {
		self.auth.handle().clone()
	}

	/// Answer every token the client presents yourself, instead of the default acceptor:
	/// on its own AUTH stream, or riding one of its moq-transport SUBSCRIBE or
	/// PUBLISH_NAMESPACE requests as an `AUTHORIZATION TOKEN`.
	///
	/// Without it the session grants the client what its own origin handles allow for the
	/// connection's credential, and refuses any other token as unsupported, along with the
	/// request that carried it. A later call hands the queue to the new [`auth::Requests`],
	/// ending the earlier one.
	///
	/// The client's credit bounds how many tokens wait unanswered: each AUTH token holds its
	/// stream open, and each request token holds its request's slot, until it is answered or
	/// withdrawn. A withdrawn token leaves the queue.
	///
	/// [`auth::Requests`]: crate::auth::Requests
	pub fn requests(&mut self) -> crate::auth::Requests {
		self.auth.handle().requests()
	}

	/// The connection credential the client presented in its SETUP: moq-transport's
	/// `AUTHORIZATION TOKEN` option, or moq-lite-07's Token parameter.
	///
	/// Older moq-lite versions carry none, so they return `None`. The transport has not
	/// verified it: authorize on it the way you would a URL token.
	pub fn token(&self) -> Option<&setup::Token> {
		self.token.as_ref()
	}

	/// Publish to the connected client. Overrides any value from the [`Server`]
	/// builder; typically set after inspecting [`path`](Self::path).
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.inner_mut().server.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client. Overrides any value from the [`Server`] builder.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.inner_mut().server.subscribe = Some(subscribe);
		self
	}

	/// Assign the identity this peer's routes are attributed to, overriding the fresh
	/// per-session default.
	///
	/// Only for a peer whose identity the server has actually established, such as one
	/// authenticated by mTLS or a token ([`crate::Client::with_peer_hop`] is the
	/// dialing-side equivalent). An identity the peer declares on the wire still wins.
	///
	/// Two sessions given the same origin are treated as one endpoint: routes learned
	/// from either are kept off both, and content arriving on either is interchangeable
	/// with the other's. That is the point when they really are one peer reconnecting or
	/// running redundant links, and a bug otherwise. Derive it from the authenticated
	/// identity, never from something coarser like the remote address.
	pub fn with_peer_hop(mut self, hop: crate::Hop) -> Self {
		self.assigned_hop = hop;
		self
	}

	/// Set the per-connection [`stats::Session`] context. Overrides any value from the
	/// [`Server`] builder.
	pub fn with_stats(mut self, stats: stats::Session) -> Self {
		self.inner_mut().server.stats = stats;
		self
	}

	/// Set the session's limits after authorizing the peer and before answering SETUP.
	pub fn with_limits(mut self, limits: crate::session::Limits) -> Self {
		self.inner_mut().server.limits = limits;
		self
	}

	/// Refuse the session with [`Error::Timeout`] if [`ok`](Self::ok) has not answered
	/// SETUP by `at`, as when the peer withholds flow-control credit for the reply.
	///
	/// Dropping a pending `ok` refuses it the same way, so a caller's own deadline counts too.
	pub fn with_deadline(mut self, at: Instant) -> Self {
		self.inner_mut().deadline = Some(at);
		self
	}

	fn inner_mut(&mut self) -> &mut RequestInner {
		self.inner.as_mut().expect("request already responded")
	}

	/// Accept the session, returning the [`Session`] and its [`Driver`](crate::Driver).
	///
	/// Poll or spawn the returned driver to run the session.
	pub async fn ok(mut self) -> Result<(Session, crate::Driver), Error> {
		let peer_hop = Some(self.assigned_hop);
		let auth = self.auth.clone();
		let RequestInner {
			server,
			runtime,
			deadline,
			handshake,
		} = self.inner.take().expect("request already responded");

		let stats = server.stats.clone();
		let (session, version) = handshake.transport();
		let unanswered = Unanswered(Some((session, version, stats.clone())));
		let reply = async move {
			match handshake {
				PausedHandshake::LiteBare { session, version } => {
					server.start_lite(runtime, session, version, None, peer_hop, auth)
				}
				PausedHandshake::LiteSetup {
					session,
					version,
					client_setup,
				} => {
					server.stats.control(stats::Message::Session, client_setup.size, true);
					server.start_lite(runtime, session, version, Some(client_setup), peer_hop, auth)
				}
				PausedHandshake::Ietf(paused) => paused.ok(server, runtime, peer_hop, auth),
				// Boxed so the bidi SETUP write of these old versions doesn't size every caller's future.
				PausedHandshake::Negotiated(paused) => Box::pin(paused.ok(server, runtime, peer_hop, auth)).await,
			}
		};
		let res = match deadline {
			// The guard refuses the session on the way out.
			Some(at) => kio::clock::timeout_at(at, reply).await.map_err(|_| Error::Timeout)?,
			None => reply.await,
		};
		unanswered.answered();
		if let Err(err) = &res {
			stats.end(err);
		}
		res
	}

	/// Reject the session, closing the transport with `err`'s wire code.
	pub fn close(mut self, err: Error) {
		let inner = self.inner.take().expect("request already responded");
		inner.close(err);
	}
}

impl RequestInner {
	fn close(self, err: Error) {
		self.server.stats.end(&err);
		let (mut session, version) = self.handshake.transport();
		close(&mut session, &err, Some(version));
	}
}

/// A SETUP reply the peer has not taken yet. Dropped unanswered, it refuses the session with
/// [`Error::Timeout`]: the reply waits only on the peer, so giving up on it means the peer
/// took too long.
struct Unanswered(Option<(Transport, Version, stats::Session)>);

impl Unanswered {
	fn answered(mut self) {
		self.0 = None;
	}
}

impl Drop for Unanswered {
	fn drop(&mut self) {
		if let Some((mut session, version, stats)) = self.0.take() {
			close(&mut session, &Error::Timeout, Some(version));
			stats.end(&Error::Timeout);
		}
	}
}

impl PausedHandshake {
	/// The transport, and the version whose codes close it.
	fn transport(&self) -> (Transport, Version) {
		match self {
			Self::LiteBare { session, version } | Self::LiteSetup { session, version, .. } => {
				(session.clone(), (*version).into())
			}
			Self::Ietf(paused) => (paused.session.clone(), paused.version.into()),
			Self::Negotiated(paused) => (paused.session.clone(), paused.version.into()),
		}
	}
}

/// Close `session` with `err`'s wire code.
fn close(session: &mut Transport, err: &Error, version: Option<Version>) {
	let error = SessionError::from(err);
	let code = version.map_or_else(|| error.to_code(), |version| version.session_code(&error));
	session.close(code, &err.to_string());
}

impl Drop for Handshake {
	// A dropped request would otherwise leave the client hanging until its idle
	// timeout: it already sent SETUP and is waiting on a response. Reject loudly.
	fn drop(&mut self) {
		if let Some(inner) = self.inner.take() {
			tracing::warn!("Handshake dropped without ok() or close(); rejecting the session");
			inner.close(Error::Cancel);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Hop;
	use crate::model::ProduceTest;
	use std::{
		collections::VecDeque,
		sync::{Arc, Mutex},
	};

	use crate::ALPN_LITE_05;
	use bytes::Bytes;

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	#[derive(Debug, Clone, Default)]
	struct FakeError;
	impl std::fmt::Display for FakeError {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "fake transport error")
		}
	}
	impl std::error::Error for FakeError {}
	impl crate::transport::Error for FakeError {
		fn session_error(&self) -> Option<(u32, String)> {
			Some((0, "closed".to_string()))
		}
	}

	/// A session that replays a queue of streams (each a `Vec<u8>`) in order from
	/// `accept_uni` and `accept_bi`, and records the code it was closed with; everything
	/// else is inert.
	#[derive(Clone)]
	struct FakeSession {
		address: crate::transport::Address,
		protocol: Option<&'static str>,
		uni: Arc<Mutex<VecDeque<Vec<u8>>>>,
		bi: Arc<Mutex<VecDeque<Vec<u8>>>>,
		closed: Arc<Mutex<Option<u32>>>,
		stops: Arc<Mutex<Vec<u32>>>,
		/// The peer grants no credit, so nothing this side writes is ever taken.
		withheld: bool,
	}

	impl FakeSession {
		fn new(protocol: &'static str, uni: impl IntoIterator<Item = Vec<u8>>) -> Self {
			Self {
				address: crate::transport::Address::Setup,
				protocol: Some(protocol),
				uni: Arc::new(Mutex::new(uni.into_iter().collect())),
				bi: Default::default(),
				closed: Default::default(),
				stops: Default::default(),
				withheld: false,
			}
		}

		fn withholding_credit(mut self) -> Self {
			self.withheld = true;
			self
		}

		fn with_bi(self, bi: Vec<u8>) -> Self {
			self.bi.lock().unwrap().push_back(bi);
			self
		}

		fn close_code(&self) -> Option<u32> {
			*self.closed.lock().unwrap()
		}
	}

	impl crate::transport::poll::Session for FakeSession {
		type SendStream = FakeSend;
		type RecvStream = FakeRecv;
		type Error = FakeError;

		fn poll_accept_uni(&self, _waiter: &kio::Waiter) -> std::task::Poll<Result<Self::RecvStream, Self::Error>> {
			match self.uni.lock().unwrap().pop_front() {
				Some(data) => std::task::Poll::Ready(Ok(crate::transport::poll::Buffered::new(FakeChunks {
					data: data.into(),
					stops: self.stops.clone(),
				}))),
				None if self.close_code().is_some() => std::task::Poll::Ready(Err(FakeError)),
				None => std::task::Poll::Pending,
			}
		}
		fn poll_accept_bi(
			&self,
			_waiter: &kio::Waiter,
		) -> std::task::Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			match self.bi.lock().unwrap().pop_front() {
				Some(data) => std::task::Poll::Ready(Ok((
					FakeSend {
						withheld: self.withheld,
					},
					crate::transport::poll::Buffered::new(FakeChunks {
						data: data.into(),
						stops: self.stops.clone(),
					}),
				))),
				None => std::task::Poll::Pending,
			}
		}
		fn poll_open_bi(
			&self,
			_waiter: &kio::Waiter,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> std::task::Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			std::task::Poll::Pending
		}

		fn poll_open_request(
			&self,
			waiter: &kio::Waiter,
			options: crate::transport::poll::SendOptions<
				'_,
				<Self::SendStream as crate::transport::poll::SendStream>::Group,
			>,
		) -> std::task::Poll<Result<Option<crate::transport::poll::BiStreams<Self>>, Self::Error>> {
			self.poll_open_bi(waiter, options).map_ok(Some)
		}
		fn poll_open_uni(
			&self,
			_waiter: &kio::Waiter,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
			std::task::Poll::Pending
		}
		fn send_group(&self, _: i64) -> Option<crate::transport::poll::Ungrouped> {
			None
		}

		fn send_datagram(
			&self,
			_: Bytes,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> Result<(), Self::Error> {
			Ok(())
		}
		fn poll_recv_datagram(&self, _waiter: &kio::Waiter) -> std::task::Poll<Result<Bytes, Self::Error>> {
			std::task::Poll::Pending
		}
		fn max_datagram_size(&self) -> usize {
			1200
		}
		fn address(&self) -> crate::transport::Address {
			self.address
		}
		fn protocol(&self) -> Option<&str> {
			self.protocol
		}
		fn close(&self, code: u32, _reason: &str) {
			self.closed.lock().unwrap().get_or_insert(code);
		}
		fn poll_closed(&self, _waiter: &kio::Waiter) -> std::task::Poll<Self::Error> {
			std::task::Poll::Pending
		}
	}

	#[derive(Clone, Default)]
	struct FakeSend {
		withheld: bool,
	}
	impl crate::transport::poll::SendStream for FakeSend {
		type Error = FakeError;
		type Group = crate::transport::poll::Ungrouped;
		fn poll_write(&mut self, _waiter: &kio::Waiter, buf: &[u8]) -> std::task::Poll<Result<usize, Self::Error>> {
			match self.withheld {
				true => std::task::Poll::Pending,
				false => std::task::Poll::Ready(Ok(buf.len())),
			}
		}
		fn set_priority(&mut self, _order: i64) {}
		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
		fn reset(&mut self, _code: u32) {}
		fn poll_closed(&self, _waiter: &kio::Waiter) -> std::task::Poll<Result<(), Self::Error>> {
			std::task::Poll::Ready(Ok(()))
		}
	}

	type FakeRecv = crate::transport::poll::Buffered<FakeChunks>;

	struct FakeChunks {
		data: VecDeque<u8>,
		stops: Arc<Mutex<Vec<u32>>>,
	}
	impl crate::transport::poll::Chunks for FakeChunks {
		type Error = FakeError;
		fn poll_read_chunk(
			&mut self,
			_waiter: &kio::Waiter,
			max: usize,
		) -> std::task::Poll<Result<Option<Bytes>, Self::Error>> {
			if self.data.is_empty() {
				return std::task::Poll::Ready(Ok(None));
			}
			let size = max.min(self.data.len());
			std::task::Poll::Ready(Ok(Some(self.data.drain(..size).collect())))
		}
		fn stop(&mut self, code: u32) {
			self.stops.lock().unwrap().push(code);
		}
	}

	/// Encode a lite-05 Setup Stream: the `DataType::Setup` tag then the SETUP message.
	fn lite05_setup(path: Option<&str>, role: Option<Role>, hop: Option<Hop>) -> Vec<u8> {
		let v = lite::Version::Lite05;
		let mut buf = Vec::new();
		lite::DataType::Setup
			.encode(&mut crate::coding::Encoder::new(&mut buf, v.into()), v)
			.unwrap();
		lite::Setup {
			probe: lite::ProbeLevel::None,
			path: path.map(str::to_string),
			role,
			cost: None,
			hop,
			token: None,
			ietf: false,
			implementation: None,
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, v.into()), v)
		.unwrap();
		buf
	}

	/// Encode a moq-transport Setup Stream: the unified SETUP message, whose parameters
	/// carry the request path the same way lite-05's does.
	fn ietf_setup(version: ietf::Version, path: Option<&str>) -> Vec<u8> {
		let mut params = ietf::Parameters::default();
		if let Some(path) = path {
			params.set_bytes(ietf::ParameterBytes::Path, path.as_bytes().to_vec());
		}
		ietf_setup_with(version, params)
	}

	fn ietf_setup_with(version: ietf::Version, params: ietf::Parameters) -> Vec<u8> {
		let parameters = params.encode_bytes(version).unwrap();

		let mut buf = Vec::new();
		setup::Setup { parameters }
			.encode(
				&mut crate::coding::Encoder::new(&mut buf, (crate::Version::Ietf(version)).into()),
				crate::Version::Ietf(version),
			)
			.unwrap();
		buf
	}

	/// A moq-lite-02 CLIENT_SETUP, sent on the bidi stream the `moql` ALPN negotiates over.
	fn moql_setup() -> Vec<u8> {
		let version = crate::Version::Lite(lite::Version::Lite02);
		let mut buf = Vec::new();
		setup::Client {
			versions: crate::coding::Versions::from([version.into()]),
			parameters: Bytes::new(),
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
		.unwrap();
		buf
	}

	fn setup_token() -> setup::Token {
		setup::Token {
			kind: setup::Token::OUT_OF_BAND,
			value: vec![0x00, 0xff, b'j', b'w', b't'],
		}
	}

	fn token_params(version: ietf::Version) -> ietf::Parameters {
		let mut params = ietf::Parameters::default();
		ietf::token::into_setup(&mut params, &setup_token(), version).unwrap();
		params
	}

	/// An acceptor taken with `Handshake::requests()` BEFORE `ok()` must govern the
	/// session the driver then runs, through the REAL accept path (`accept_request` -> `ok`).
	/// The session's own `auth()` (the handle the driver polls) routes a request token to that
	/// pre-ok consumer instead of falling to the `Unsupported` default, and granting admits.
	/// A QUIC server has no other point to install the verifier.
	#[kio::test]
	async fn a_pre_ok_verifier_governs_the_session() {
		let cases = [(
			FakeSession::new(ALPN_18, [ietf_setup(ietf::Version::Draft18, Some("room/alice"))]),
			ietf::Version::Draft18,
		)];

		for (session, version) in cases {
			let mut request = Server::new()
				.accept_request(kio::clock::now(), session)
				.await
				.unwrap_or_else(|e| panic!("accept at {version:?}: {e}"));

			// Install the verifier before ok(), as a QUIC server must.
			let mut requests = request.requests();
			let (net_session, _driver) = request.ok().await.unwrap_or_else(|e| panic!("ok at {version:?}: {e}"));

			// The session's own handle is the one the driver polls. Verifying a request token on
			// it must reach the pre-ok consumer; if the handle were not shared it would fall to
			// the Unsupported default and the consumer would never see this request.
			let verdict = net_session.auth().verify_request(
				crate::setup::Token {
					kind: crate::setup::Token::OUT_OF_BAND,
					value: b"jwt".to_vec(),
				},
				None,
			);
			let received = requests.next().await.expect("the pre-ok consumer receives the request");
			assert_eq!(received.token().as_ref(), b"jwt", "{version:?}");
			let _issued = received.accept(crate::auth::Grant::all());
			assert!(verdict.grant().await.is_ok(), "granting admits at {version:?}");
		}
	}

	/// A peer that never takes the SETUP reply is refused at the handshake's deadline
	/// with the timeout code, and the session's stats count it.
	#[kio::test]
	async fn a_reply_the_peer_never_takes_fails_at_the_deadline() {
		let registry = stats::Registry::new(Default::default());
		let version = Version::Lite(lite::Version::Lite02);
		let session = FakeSession::new(ALPN_LITE, [])
			.with_bi(moql_setup())
			.withholding_credit();
		let start = kio::clock::now();
		let deadline = start + std::time::Duration::from_secs(10);
		let request = Server::new()
			.accept_request(start, session.clone())
			.await
			.unwrap()
			.with_stats(
				registry
					.tier(Default::default())
					.session(crate::Path::from_static("room")),
			)
			.with_deadline(deadline);
		let Err(err) = request.ok().await else {
			panic!("a reply the peer never took completed");
		};
		assert!(matches!(err, Error::Timeout), "{err:?}");
		assert_eq!(kio::clock::now(), deadline);
		assert_eq!(session.close_code(), Some(version.session_code(&SessionError::Timeout)));
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(stats::Failure::Application), 1, "{failed:?}");
	}

	/// A reply the caller stops waiting for, as a relay does at its drain deadline, is
	/// refused like one past the handshake's own deadline.
	#[kio::test]
	async fn an_abandoned_reply_fails_like_the_deadline() {
		let registry = stats::Registry::new(Default::default());
		let version = Version::Lite(lite::Version::Lite02);
		let session = FakeSession::new(ALPN_LITE, [])
			.with_bi(moql_setup())
			.withholding_credit();
		let request = Server::new()
			.accept_request(kio::clock::now(), session.clone())
			.await
			.unwrap()
			.with_stats(
				registry
					.tier(Default::default())
					.session(crate::Path::from_static("room")),
			);
		let abandoned = kio::clock::timeout(std::time::Duration::from_secs(1), request.ok()).await;
		assert!(abandoned.is_err(), "a reply the peer never took completed");
		assert_eq!(session.close_code(), Some(version.session_code(&SessionError::Timeout)));
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(stats::Failure::Application), 1, "{failed:?}");
	}

	/// A rejection counts by the code the peer is told, so a "no error" close is clean
	/// however this side spelled it.
	#[kio::test]
	async fn a_rejection_counts_by_its_code() {
		for (err, failed) in [
			(Error::GoingAway, None),
			(Error::Session(SessionError::Cancel), None),
			(Error::Unauthorized, Some(stats::Failure::Application)),
		] {
			let registry = stats::Registry::new(Default::default());
			let session = FakeSession::new(ALPN_22, [ietf_setup(ietf::Version::Draft22, None)]);
			Server::new()
				.accept_request(kio::clock::now(), session)
				.await
				.unwrap()
				.with_stats(
					registry
						.tier(Default::default())
						.session(crate::Path::from_static("room")),
				)
				.close(err.clone());
			let failures = registry.snapshot().sessions()[0].1.sessions_failed;
			for kind in stats::Failure::ALL {
				let expected = u64::from(failed == Some(*kind));
				assert_eq!(failures.count(*kind), expected, "{err:?}: {kind:?}");
			}
		}
	}

	#[kio::test]
	async fn accept_request_exposes_the_setup_token() {
		let ietf = FakeSession::new(
			ALPN_22,
			[ietf_setup_with(
				ietf::Version::Draft22,
				token_params(ietf::Version::Draft22),
			)],
		);
		let request = Server::new().accept_request(kio::clock::now(), ietf).await.unwrap();
		assert_eq!(request.token(), Some(&setup_token()));
	}

	#[kio::test]
	async fn accept_request_without_a_token_reports_none() {
		let ietf = FakeSession::new(ALPN_22, [ietf_setup(ietf::Version::Draft22, None)]);
		let lite = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		for (name, session) in [("draft-22", ietf), ("lite-05", lite)] {
			let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
			assert_eq!(request.token(), None, "{name}");
		}
	}

	/// A SETUP the server refuses closes the session with the code naming why.
	#[kio::test]
	async fn a_refused_setup_token_closes_with_its_code() {
		let delete = [0x0, 0x7]; // DELETE alias 7
		let truncated = [0x3]; // USE_VALUE with no Token Type
		for (raw, code) in [
			(&delete[..], SessionError::ProtocolViolation),
			(&truncated[..], SessionError::KeyValueFormatting),
		] {
			let mut params = ietf::Parameters::default();
			params.set_bytes(ietf::ParameterBytes::AuthorizationToken, raw.to_vec());

			let session = FakeSession::new(ALPN_22, [ietf_setup_with(ietf::Version::Draft22, params)]);
			let result = Server::new().accept_request(kio::clock::now(), session.clone()).await;
			assert!(result.is_err());
			assert_eq!(session.close_code(), Some(code.to_code()), "{code}");
		}
	}

	#[kio::test]
	async fn accept_request_reads_ietf_path() {
		// Every draft gates on the SETUP stream before starting, so the
		// path is known at authorization time just like lite-05.
		for (alpn, version) in [(ALPN_18, ietf::Version::Draft18), (ALPN_22, ietf::Version::Draft22)] {
			let session = FakeSession::new(alpn, [ietf_setup(version, Some("/team/room"))]);
			let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
			assert_eq!(request.path(), "/team/room", "{alpn}");
		}
	}

	#[kio::test]
	async fn accept_request_ietf_without_path_is_empty() {
		let session = FakeSession::new(ALPN_22, [ietf_setup(ietf::Version::Draft22, None)]);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[kio::test]
	async fn accept_request_ietf_empty_path_is_accepted() {
		let session = FakeSession::new(ALPN_22, [ietf_setup(ietf::Version::Draft22, Some(""))]);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	/// Encode a lite-05 GROUP uni stream header (just the `DataType::Group` tag).
	fn lite05_group() -> Vec<u8> {
		let mut buf = Vec::new();
		lite::DataType::Group
			.encode(
				&mut crate::coding::Encoder::new(&mut buf, lite::Version::Lite05.into()),
				lite::Version::Lite05,
			)
			.unwrap();
		buf
	}

	#[kio::test]
	async fn accept_request_reads_lite05_path() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/team/room"), None, None)]);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert_eq!(request.role(), None, "a client that omits the role is bidirectional");
	}

	#[kio::test]
	async fn accept_request_lite05_without_path_is_empty() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[kio::test]
	async fn accept_request_lite05_empty_path_is_accepted() {
		// An empty path is valid on the wire and means the same as omitting it, so a
		// client that wants the root doesn't have to special-case the parameter.
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some(""), None, None)]);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[kio::test]
	async fn uri_address_rejects_setup_path_presence() {
		for version in [
			Version::Lite(lite::Version::Lite05),
			Version::Lite(lite::Version::Lite06),
			Version::Lite(lite::Version::Lite07),
			Version::Ietf(ietf::Version::Draft18),
			Version::Ietf(ietf::Version::Draft22),
		] {
			let alpn = version.alpn();
			for path in ["", "/team/room?jwt=other"] {
				let setup = match version {
					Version::Lite(_) => lite05_setup(Some(path), None, None),
					Version::Ietf(version) => ietf_setup(version, Some(path)),
				};
				let mut session = FakeSession::new(alpn, [setup]);
				session.address = crate::transport::Address::Uri;
				let observed = session.clone();
				let result = Server::new()
					.with_versions(version.into())
					.accept_request(kio::clock::now(), session)
					.await;
				assert!(
					matches!(result, Err(Error::ProtocolViolation)),
					"{alpn}: URI address accepted a SETUP path"
				);
				assert_eq!(observed.close_code(), Some(3));
			}
		}
	}

	#[kio::test]
	async fn uri_address_accepts_setup_without_path() {
		for version in [
			Version::Lite(lite::Version::Lite05),
			Version::Lite(lite::Version::Lite06),
			Version::Lite(lite::Version::Lite07),
			Version::Ietf(ietf::Version::Draft18),
			Version::Ietf(ietf::Version::Draft22),
		] {
			let alpn = version.alpn();
			let setup = match version {
				Version::Lite(_) => lite05_setup(None, None, None),
				Version::Ietf(version) => ietf_setup(version, None),
			};
			let mut session = FakeSession::new(alpn, [setup]);
			session.address = crate::transport::Address::Uri;
			let request = Server::new()
				.with_versions(version.into())
				.accept_request(kio::clock::now(), session)
				.await
				.unwrap();
			assert_eq!(request.path(), "");
		}
	}

	#[kio::test]
	async fn accept_request_reads_lite05_role() {
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_setup(Some("/team/room"), Some(Role::Publisher), None)],
		);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.role(), Some(Role::Publisher));
	}

	#[kio::test]
	async fn accept_request_holds_uni_stream_before_setup() {
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_group(), lite05_setup(Some("/team/room"), None, None)],
		);
		let stops = session.stops.clone();
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert!(stops.lock().unwrap().is_empty(), "the early stream must stay open");
	}

	#[kio::test]
	async fn accept_request_reads_buffered_setup_after_transport_close() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/closed"), None, None)]);
		crate::transport::poll::Session::close(&session, SessionError::Cancel.to_code(), "closed");
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "/closed");
	}

	#[kio::test]
	async fn accepted_lite_setup_refuses_a_second_setup_stream() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let transport = session.clone();
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		let (_session, mut driver) = request.ok().await.unwrap();
		transport.uni.lock().unwrap().push_back(vec![1]);
		let _ = driver.poll(kio::clock::now(), &kio::Waiter::noop());
		assert_eq!(transport.close_code(), Some(SessionError::ProtocolViolation.to_code()));
	}

	#[kio::test]
	async fn accept_request_reads_lite05_peer_hop() {
		let hop = Hop::new(42).unwrap();
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, Some(hop))]);
		let request = Server::new().accept_request(kio::clock::now(), session).await.unwrap();
		assert_eq!(request.peer_hop(), Some(hop));
	}

	#[kio::test]
	async fn anonymous_peer_hop_filters_routes_from_server_session() {
		let other = Hop::new(778).unwrap();
		let origin = crate::origin::Config::new(Hop::new(1).unwrap()).produce();

		let gate = kio::Producer::new(true);
		let transport = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = transport.log.clone();
		let version = ietf::Version::Draft18;
		let request = Handshake {
			path: None,
			role: None,
			origin: None,
			token: None,
			peer_implementation: None,
			assigned_hop: Hop::random(),
			auth: crate::auth::Link::ietf(false),
			inner: Some(RequestInner {
				server: Server::new().with_publisher(&origin),
				runtime: Deadlines::new(kio::clock::now()),
				deadline: None,
				handshake: PausedHandshake::Ietf(PausedIetf {
					session: Transport::new(transport),
					version,
					peer_setup: ietf::PeerSetup {
						stream: crate::coding::Reader::new(
							crate::transport::boxed::RecvStream::new(crate::lite::test_transport::PendingRecv),
							Version::Ietf(version),
						),
						size: 0,
						options: ietf::SetupOptions {
							path: None,
							token: None,
							implementation: None,
							declared: ietf::peer::Peer::default(),
						},
						early: Vec::new(),
					},
				}),
			}),
		};
		let assigned = request.assigned_hop;

		let mut echoed_hops = crate::Hops::new();
		echoed_hops.push(crate::Hop::UNKNOWN).unwrap();
		let _echoed = origin
			.announce(
				"echoed-route",
				crate::origin::Route::default()
					.with_hops(echoed_hops)
					.with_via(assigned),
			)
			.unwrap();

		let mut local_hops = crate::Hops::new();
		local_hops.push(other).unwrap();
		let _local = origin
			.announce("local-route", crate::origin::Route::default().with_hops(local_hops))
			.unwrap();

		let (session, driver) = request.ok().await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));

		for _ in 0..100 {
			if occurrences(&log, b"local-route") > 0 {
				break;
			}
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert_eq!(occurrences(&log, b"echoed-route"), 0);
		assert_eq!(occurrences(&log, b"local-route"), 1);
		drop(session);
	}
}
