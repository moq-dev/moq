//! Accepting a MoQ session, including the paused handshake that inspects the
//! peer's SETUP before granting origins.

use crate::driver::Transport;
use crate::origin;
use crate::time::{Deadlines, Instant};
use crate::transport::poll::Session as _;
use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_20, ALPN_21, ALPN_22, ALPN_LITE, ALPN_LITE_03,
	ALPN_LITE_04, ALPN_LITE_05, ALPN_LITE_06, ALPN_LITE_07_WIP, Consume, Error, NEGOTIATED, Role, Session,
	SessionError, Version, Versions,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup, stats,
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
			if self.state.unused().await.is_err() {
				return;
			}
			match self.state.write_unused() {
				kio::Unused::Idle(state) => return state.close(),
				// Admitted between the wake and the commit: wait for it too.
				kio::Unused::Used => continue,
				kio::Unused::Closed => return,
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
	/// traffic counters are attributed through the model for this session's lifetime.
	/// Pass [`stats::Session::default`] (a no-op context) to opt out.
	pub fn with_stats(mut self, stats: stats::Session) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and subscribe from one shared [`origin::Producer`].
	pub fn with_origin(self, origin: origin::Producer) -> Self {
		self.with_publisher(&origin).with_subscriber(origin)
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
		client_setup: Option<lite::AcceptedSetup<Transport>>,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Handle,
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
				ietf: self.extensions.ietf && version.has_ietf(),
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
				estimate: Some(start.estimate),
				bandwidth: crate::bandwidth::Allocator::default(),
			},
		))
	}

	async fn handshake_lite(&self, runtime: Deadlines, mut session: Transport) -> Result<Handshake, Error> {
		let (path, role, origin, handshake) = match session.protocol() {
			Some(alpn @ (ALPN_LITE_05 | ALPN_LITE_06 | ALPN_LITE_07_WIP)) => {
				let version = match alpn {
					ALPN_LITE_07_WIP => lite::Version::Lite07,
					ALPN_LITE_06 => lite::Version::Lite06,
					_ => lite::Version::Lite05,
				};
				self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
				// Gate on the client's SETUP: read it before serving so the
				// caller can scope by the advertised path. Seeded back into
				// `start` on `ok()` so PROBE gating resolves without
				// re-reading the (consumed) Setup Stream.
				let client_setup = lite::accept_setup(&mut session, version).await?;
				(
					client_setup.setup.path.clone(),
					client_setup.setup.role,
					client_setup.setup.hop,
					PausedHandshake::LiteSetup {
						session,
						version,
						client_setup,
					},
				)
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;
				(
					None,
					None,
					None,
					PausedHandshake::LiteBare {
						session,
						version: lite::Version::Lite04,
					},
				)
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;
				(
					None,
					None,
					None,
					PausedHandshake::LiteBare {
						session,
						version: lite::Version::Lite03,
					},
				)
			}
			_ => return Err(Error::Version),
		};

		let auth = crate::auth::Handle::new(match &handshake {
			PausedHandshake::LiteBare { version, .. } | PausedHandshake::LiteSetup { version, .. } => {
				version.has_auth()
			}
			PausedHandshake::IetfModern(_) | PausedHandshake::Legacy(_) => false,
		});
		Ok(Handshake {
			path,
			role,
			origin,
			// moq-lite carries no SETUP token.
			token: None,
			assigned_hop: crate::Hop::random(),
			auth,
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
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
		let (encoding, supported) = match session.protocol() {
			Some(alpn @ (ALPN_22 | ALPN_21 | ALPN_20 | ALPN_19 | ALPN_18 | ALPN_17)) => {
				let draft = match alpn {
					ALPN_22 => ietf::Version::Draft22,
					ALPN_21 => ietf::Version::Draft21,
					ALPN_20 => ietf::Version::Draft20,
					ALPN_19 => ietf::Version::Draft19,
					ALPN_18 => ietf::Version::Draft18,
					_ => ietf::Version::Draft17,
				};

				self.versions.select(Version::Ietf(draft)).ok_or(Error::Version)?;
				return self.accept_ietf_modern(runtime, session, draft).await;
			}
			Some(ALPN_16) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft16))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_15) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft15))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_14) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft14))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_LITE_07_WIP | ALPN_LITE_06 | ALPN_LITE_05 | ALPN_LITE_04 | ALPN_LITE_03) => {
				return self.handshake_lite(runtime, session).await;
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		// Legacy bidi SETUP exchange (IETF 14-16, lite 01/02). Read the client's
		// SETUP to choose the version; `ok()` sends the server SETUP and starts.
		// Stats attach in `ok()`: a caller may still replace them on the handshake.
		let mut stream = Stream::accept(&mut session, encoding).await?;
		let (client, client_setup_size) = stream.reader.decode_sized::<setup::Client>().await?;

		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;

		// Pull the request path and max request ID out now (IETF only) so `ok()`
		// doesn't re-decode the consumed parameters. moq-transport carries the path
		// in its SETUP just like lite-05.
		let (path, token, request_id_max, peer_declared) = match version {
			Version::Ietf(v) => {
				let (params, _) = ietf::Parameters::decode_slice(&client.parameters, v)?;
				let path = match params.get_bytes(ietf::ParameterBytes::Path) {
					Some(bytes) => Some(
						std::str::from_utf8(bytes)
							.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
							.to_owned(),
					),
					None => None,
				};
				let token = ietf::token::from_setup(&params, v)?;
				let request_id_max = params
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);
				let peer_declared = ietf::peer::Peer {
					solicit: ietf::solicit::from_setup(&params, v)?,
					hidden: ietf::hidden::from_setup(&params, v),
					max_age: ietf::max_age::from_setup(&params, v),
					active_count: ietf::active_count::from_setup(&params, v),
					flate: ietf::flate::from_setup(&params, v)?,
					..Default::default()
				};
				(path, token, request_id_max, peer_declared)
			}
			Version::Lite(_) => (None, None, None, ietf::peer::Peer::default()),
		};

		Ok(Handshake {
			path,
			role: None,
			origin: None,
			token,
			assigned_hop: crate::Hop::random(),
			// Lite 01/02 and moq-transport 14-16 carry no AUTH, though a moq-transport request
			// still carries a token.
			auth: match version {
				Version::Ietf(_) => crate::auth::Handle::ietf(false),
				Version::Lite(_) => crate::auth::Handle::new(false),
			},
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake: PausedHandshake::Legacy(PausedLegacy {
					session,
					stream,
					version,
					request_id_max,
					peer_declared,
					client_setup_size,
				}),
			}),
		})
	}

	/// Read a draft-17/18 client's SETUP (with its request path) off its uni stream,
	/// then pause. `ok()` starts the session and hands the stream back for GOAWAY.
	async fn accept_ietf_modern(
		&self,
		runtime: Deadlines,
		mut session: Transport,
		version: ietf::Version,
	) -> Result<Handshake, Error> {
		let peer_setup = ietf::accept_setup(&mut session, version).await?;
		Ok(Handshake {
			path: peer_setup.path.clone(),
			role: None,
			// A moq-transport peer only has an identity if it negotiated the MoQ
			// Cluster extension and declared a non-zero Hop ID.
			origin: peer_setup.declared.cluster.hop.filter(|h| *h != crate::Hop::UNKNOWN),
			token: peer_setup.token.clone(),
			assigned_hop: crate::Hop::random(),
			// The client's SETUP already settled whether it offered MoQ Auth, so it is
			// negotiated exactly when we offer it too.
			auth: crate::auth::Handle::ietf(peer_setup.declared.auth && self.extensions.auth),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake: PausedHandshake::IetfModern(PausedIetfModern {
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
	/// The identity this session's routes are attributed to for split horizon when the
	/// peer declares none on the wire; it never enters a hop chain. Fresh per request unless the caller overrides it
	/// ([`Handshake::with_peer_hop`]).
	assigned_hop: crate::Hop,
	/// The session's auth handle, available before [`Handshake::ok`] so the caller can
	/// take the peer's token requests before any arrive.
	auth: crate::auth::Handle,
	// Taken by `ok`/`close`; `Drop` rejects the handshake if neither ran.
	inner: Option<RequestInner>,
}

/// The parts of a [`Handshake`] consumed by [`Handshake::ok`] / [`Handshake::close`].
struct RequestInner {
	server: Server,
	/// Supplies the clock and timers for the accepted session.
	runtime: Deadlines,
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
		client_setup: lite::AcceptedSetup<Transport>,
	},
	IetfModern(PausedIetfModern),
	Legacy(PausedLegacy),
}

/// Modern IETF (17+): the client's SETUP (with its request path) has been
/// read off its uni stream; `ok` starts the session, handing that stream back
/// for GOAWAY monitoring.
struct PausedIetfModern {
	session: Transport,
	version: ietf::Version,
	peer_setup: ietf::PeerSetup<Transport>,
}

impl PausedIetfModern {
	/// Complete the handshake with the final server config.
	fn ok(
		self,
		server: Server,
		runtime: Deadlines,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Handle,
	) -> Result<(Session, crate::Driver), Error> {
		let Self {
			session,
			version,
			peer_setup,
		} = self;
		server.stats.control(stats::Control::Session, peer_setup.size, true);
		let (publish, subscribe) = server.stat_tagged_origins();

		// The client's SETUP was read at the pause; hand the stream back
		// for GOAWAY. A server never advertises a path, hence `None`.
		let (protocol, goaway) = ietf::start(ietf::Config {
			runtime: runtime.clone(),
			stats: server.stats.clone(),
			limits: server.limits,
			session: session.clone(),
			setup: None,
			request_id_max: None,
			client: false,
			publish,
			subscribe,
			peer_hop,
			// Only the dialing side prices a link.
			cost: None,
			version,
			path: None,
			authority: None,
			peer_setup_stream: Some(peer_setup.stream),
			peer_declared: Some(peer_setup.declared),
			auth: auth.clone(),
			early_unis: peer_setup.early,
			extensions: server.extensions,
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
				estimate: None,
				bandwidth: crate::bandwidth::Allocator::default(),
			},
		))
	}
}

/// Legacy IETF (draft 14-16) and lite 01/02: the client SETUP has been read
/// off the bidi stream (including its request path) but the server SETUP
/// hasn't been sent; `ok` finishes it.
struct PausedLegacy {
	session: Transport,
	stream: Stream<Transport, Version>,
	version: Version,
	request_id_max: Option<ietf::RequestId>,
	/// What the client's SETUP declared, for the options `ok` acts on.
	peer_declared: ietf::peer::Peer,
	/// The client SETUP's encoded size, counted under the final stats.
	client_setup_size: usize,
}

impl PausedLegacy {
	/// Send the server SETUP and start the session.
	async fn ok(
		self,
		server: Server,
		runtime: Deadlines,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Handle,
	) -> Result<(Session, crate::Driver), Error> {
		let Self {
			session,
			stream,
			version,
			request_id_max,
			peer_declared,
			client_setup_size,
		} = self;
		let mut stream = stream.with_stats(&server.stats);
		server.stats.control(stats::Control::Session, client_setup_size, true);
		let (publish, subscribe) = server.stat_tagged_origins();

		// Encode parameters using the version-appropriate type.
		let parameters = match version {
			Version::Ietf(v) => {
				let mut parameters = ietf::Parameters::default();
				// The client's requests, admitted up to our limits and granted back as they close.
				parameters.set_varint(
					ietf::ParameterVarInt::MaxRequestId,
					ietf::initial_max_request_id(server.limits.requests(), true),
				);
				parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
				if server.extensions.solicit {
					ietf::solicit::into_setup(&mut parameters, v);
				}
				ietf::hidden::into_setup(&mut parameters, v);
				ietf::max_age::into_setup(&mut parameters, v);
				ietf::active_count::into_setup(&mut parameters, v);
				ietf::flate::into_setup(&mut parameters, v);
				parameters.encode_bytes(v)?
			}
			Version::Lite(v) => lite::Parameters::default().encode_bytes(v)?,
		};

		let server_setup = setup::Server {
			version: version.into(),
			parameters,
		};
		stream.writer.encode(&server_setup).await?;

		let (recv_bw, protocol, goaway, auth, estimate) = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
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
					version: v,
					our_setup: lite::Setup::default(),
					peer_setup: None,
					auth,
				})?;
				(
					start.recv_bandwidth,
					crate::driver::Protocol::Lite(Box::new(start.driver)),
					start.goaway,
					start.auth,
					Some(start.estimate),
				)
			}
			Version::Ietf(v) => {
				let stream = stream.with_version(v);
				// Draft 14-16: path came in the bidi SETUP, no uni SETUP to hand back.
				let (protocol, goaway) = ietf::start(ietf::Config {
					runtime: runtime.clone(),
					stats: server.stats.clone(),
					limits: server.limits,
					session: session.clone(),
					setup: Some(stream),
					request_id_max,
					client: false,
					publish,
					subscribe,
					peer_hop,
					cost: None,
					version: v,
					path: None,
					authority: None,
					peer_setup_stream: None,
					peer_declared: Some(peer_declared),
					auth: auth.clone(),
					early_unis: Vec::new(),
					extensions: server.extensions,
				})?;
				(None, crate::driver::Protocol::Ietf(protocol), goaway, auth, None)
			}
		};

		Ok(Session::new(
			runtime,
			session,
			version,
			recv_bw,
			protocol,
			crate::session::Handles {
				stats: server.stats.clone(),
				goaway,
				auth,
				estimate,
				bandwidth: crate::bandwidth::Allocator::default(),
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
	/// origin; a `moqt-17`+ endpoint declares it via the MoQ Cluster extension. Older
	/// versions and endpoints without one return `None`.
	///
	/// Self-declared, so treat it as a correlation hint rather than an
	/// authenticated identity: authorize on the token or client certificate.
	pub fn peer_hop(&self) -> Option<crate::Hop> {
		self.origin
	}

	/// The session's auth handle, the same one [`Session::auth`] returns once accepted.
	///
	/// Take [`requests`](crate::auth::Handle::requests) here to answer the client's
	/// tokens yourself; the choice is fixed once the session's driver first runs.
	pub fn auth(&self) -> crate::auth::Handle {
		self.auth.clone()
	}

	/// The credential the client presented in its SETUP's `AUTHORIZATION TOKEN` option.
	///
	/// Only moq-transport carries one, so moq-lite sessions return `None`. The transport
	/// has not verified it: authorize on it the way you would a URL token.
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

	/// Set request caps after authorizing the peer and before answering SETUP.
	pub fn with_limits(mut self, limits: crate::session::Limits) -> Self {
		self.inner_mut().server.limits = limits;
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
			handshake,
		} = self.inner.take().expect("request already responded");

		match handshake {
			PausedHandshake::LiteBare { session, version } => {
				server.start_lite(runtime, session, version, None, peer_hop, auth)
			}
			PausedHandshake::LiteSetup {
				session,
				version,
				client_setup,
			} => {
				server.stats.control(stats::Control::Session, client_setup.size, true);
				server.start_lite(runtime, session, version, Some(client_setup), peer_hop, auth)
			}
			PausedHandshake::IetfModern(paused) => paused.ok(server, runtime, peer_hop, auth),
			// Boxed so the bidi SETUP write of these old versions doesn't size every caller's future.
			PausedHandshake::Legacy(paused) => Box::pin(paused.ok(server, runtime, peer_hop, auth)).await,
		}
	}

	/// Reject the session, closing the transport with `err`'s wire code.
	pub fn close(mut self, err: Error) {
		let inner = self.inner.take().expect("request already responded");
		inner.close(err);
	}
}

impl RequestInner {
	fn close(self, err: Error) {
		let (mut session, version) = match self.handshake {
			PausedHandshake::LiteBare { session, version } => (session, version.into()),
			PausedHandshake::LiteSetup { session, version, .. } => (session, version.into()),
			PausedHandshake::IetfModern(paused) => (paused.session, paused.version.into()),
			PausedHandshake::Legacy(paused) => (paused.session, paused.version),
		};
		close(&mut session, &err, Some(version));
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
		binding: crate::transport::Binding,
		protocol: Option<&'static str>,
		uni: Arc<Mutex<VecDeque<Vec<u8>>>>,
		bi: Arc<Mutex<VecDeque<Vec<u8>>>>,
		closed: Arc<Mutex<Option<u32>>>,
		stops: Arc<Mutex<Vec<u32>>>,
	}

	impl FakeSession {
		fn new(protocol: &'static str, uni: impl IntoIterator<Item = Vec<u8>>) -> Self {
			Self {
				binding: crate::transport::Binding::InBand,
				protocol: Some(protocol),
				uni: Arc::new(Mutex::new(uni.into_iter().collect())),
				bi: Default::default(),
				closed: Default::default(),
				stops: Default::default(),
			}
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

		fn poll_accept_uni(
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<Self::RecvStream, Self::Error>> {
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
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			match self.bi.lock().unwrap().pop_front() {
				Some(data) => std::task::Poll::Ready(Ok((
					FakeSend,
					crate::transport::poll::Buffered::new(FakeChunks {
						data: data.into(),
						stops: self.stops.clone(),
					}),
				))),
				None => std::task::Poll::Pending,
			}
		}
		fn poll_open_bi(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> std::task::Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			std::task::Poll::Pending
		}
		fn poll_open_uni(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
			std::task::Poll::Pending
		}
		fn send_group(&mut self, _: i64) -> Option<crate::transport::poll::Ungrouped> {
			None
		}

		fn send_datagram(
			&mut self,
			_: Bytes,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> Result<(), Self::Error> {
			Ok(())
		}
		fn poll_recv_datagram(
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<Bytes, Self::Error>> {
			std::task::Poll::Pending
		}
		fn max_datagram_size(&self) -> usize {
			1200
		}
		fn binding(&self) -> crate::transport::Binding {
			self.binding
		}
		fn protocol(&self) -> Option<&str> {
			self.protocol
		}
		fn close(&mut self, code: u32, _reason: &str) {
			self.closed.lock().unwrap().get_or_insert(code);
		}
		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Error> {
			std::task::Poll::Pending
		}
		fn stats(&self) -> impl crate::transport::Stats {
			crate::transport::StatsUnavailable
		}
	}

	#[derive(Clone, Default)]
	struct FakeSend;
	impl crate::transport::poll::SendStream for FakeSend {
		type Error = FakeError;
		type Group = crate::transport::poll::Ungrouped;
		fn poll_write(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			buf: &[u8],
		) -> std::task::Poll<Result<usize, Self::Error>> {
			std::task::Poll::Ready(Ok(buf.len()))
		}
		fn set_priority(&mut self, _order: i64) {}
		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
		fn reset(&mut self, _code: u32) {}
		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
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
			_cx: &mut std::task::Context<'_>,
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
			ietf: false,
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, v.into()), v)
		.unwrap();
		buf
	}

	/// Encode a draft-17+ Setup Stream: the unified SETUP message, whose parameters
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

	/// Encode a draft 14-16 CLIENT_SETUP, sent on the control bidi stream.
	fn legacy_setup(version: ietf::Version, params: ietf::Parameters) -> Vec<u8> {
		let mut buf = Vec::new();
		setup::Client {
			versions: crate::coding::Versions::from([crate::Version::Ietf(version).into()]),
			parameters: params.encode_bytes(version).unwrap(),
		}
		.encode(
			&mut crate::coding::Encoder::new(&mut buf, (crate::Version::Ietf(version)).into()),
			crate::Version::Ietf(version),
		)
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

	/// An acceptor taken with `Request::auth().requests()` BEFORE `ok()` must govern the
	/// session the driver then runs, through the REAL accept path (`accept_request` -> `ok`).
	/// The session's own `auth()` (the handle the driver polls) routes a request token to that
	/// pre-ok consumer instead of falling to the `Unsupported` default, and granting admits.
	/// Both a no-AUTH-extension legacy session (draft-14, `Handle::new(false)`) and the modern
	/// uni-SETUP path (draft-18); a QUIC server has no other point to install the verifier.
	#[moq_clock::test]
	async fn a_pre_ok_verifier_governs_the_session() {
		let cases = [
			(
				FakeSession::new(ALPN_14, [])
					.with_bi(legacy_setup(ietf::Version::Draft14, ietf::Parameters::default())),
				ietf::Version::Draft14,
			),
			(
				FakeSession::new(ALPN_18, [ietf_setup(ietf::Version::Draft18, Some("room/alice"))]),
				ietf::Version::Draft18,
			),
		];

		for (session, version) in cases {
			let request = Server::new()
				.accept_request(moq_clock::now(), session)
				.await
				.unwrap_or_else(|e| panic!("accept at {version:?}: {e}"));

			// Install the verifier before ok(), as a QUIC server must.
			let mut requests = request.auth().requests().expect("requests() pre-ok");
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

	#[moq_clock::test]
	async fn accept_request_exposes_the_setup_token() {
		let modern = FakeSession::new(
			ALPN_19,
			[ietf_setup_with(
				ietf::Version::Draft19,
				token_params(ietf::Version::Draft19),
			)],
		);
		let legacy = FakeSession::new(ALPN_16, []).with_bi(legacy_setup(
			ietf::Version::Draft16,
			token_params(ietf::Version::Draft16),
		));
		for (name, session) in [("draft-19", modern), ("draft-16", legacy)] {
			let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
			assert_eq!(request.token(), Some(&setup_token()), "{name}");
		}
	}

	#[moq_clock::test]
	async fn accept_request_without_a_token_reports_none() {
		let ietf = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, None)]);
		let lite = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		for (name, session) in [("draft-19", ietf), ("lite-05", lite)] {
			let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
			assert_eq!(request.token(), None, "{name}");
		}
	}

	/// A SETUP the server refuses closes the session with the code naming why, on both
	/// the draft-17+ uni stream and the draft 14-16 bidi stream.
	#[moq_clock::test]
	async fn a_refused_setup_token_closes_with_its_code() {
		let delete = [0x0, 0x7]; // DELETE alias 7
		let truncated = [0x3]; // USE_VALUE with no Token Type
		for (raw, code) in [
			(&delete[..], SessionError::ProtocolViolation),
			(&truncated[..], SessionError::KeyValueFormatting),
		] {
			let mut params = ietf::Parameters::default();
			params.set_bytes(ietf::ParameterBytes::AuthorizationToken, raw.to_vec());

			let modern = FakeSession::new(ALPN_19, [ietf_setup_with(ietf::Version::Draft19, params.clone())]);
			let legacy = FakeSession::new(ALPN_16, []).with_bi(legacy_setup(ietf::Version::Draft16, params));
			for (name, session) in [("draft-19", modern), ("draft-16", legacy)] {
				let result = Server::new().accept_request(moq_clock::now(), session.clone()).await;
				assert!(result.is_err(), "{name}");
				assert_eq!(session.close_code(), Some(code.to_code()), "{name} {code}");
			}
		}
	}

	#[moq_clock::test]
	async fn accept_request_reads_ietf_path() {
		// Every draft-17+ version gates on the SETUP stream before starting, so the
		// path is known at authorization time just like lite-05.
		for (alpn, version) in [
			(ALPN_17, ietf::Version::Draft17),
			(ALPN_18, ietf::Version::Draft18),
			(ALPN_19, ietf::Version::Draft19),
		] {
			let session = FakeSession::new(alpn, [ietf_setup(version, Some("/team/room"))]);
			let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
			assert_eq!(request.path(), "/team/room", "{alpn}");
		}
	}

	#[moq_clock::test]
	async fn accept_request_ietf_without_path_is_empty() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, None)]);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[moq_clock::test]
	async fn accept_request_ietf_empty_path_is_accepted() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, Some(""))]);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
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

	#[moq_clock::test]
	async fn accept_request_reads_lite05_path() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/team/room"), None, None)]);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert_eq!(request.role(), None, "a client that omits the role is bidirectional");
	}

	#[moq_clock::test]
	async fn accept_request_lite05_without_path_is_empty() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[moq_clock::test]
	async fn accept_request_lite05_empty_path_is_accepted() {
		// An empty path is valid on the wire and means the same as omitting it, so a
		// client that wants the root doesn't have to special-case the parameter.
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some(""), None, None)]);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[moq_clock::test]
	async fn uri_binding_rejects_setup_path_presence() {
		for alpn in [ALPN_LITE_05, ALPN_LITE_06, ALPN_LITE_07_WIP] {
			for path in ["", "/team/room"] {
				let mut session = FakeSession::new(alpn, [lite05_setup(Some(path), None, None)]);
				session.binding = crate::transport::Binding::Uri;
				let observed = session.clone();
				let result = Server::new()
					.with_versions(alpn.parse::<Version>().unwrap().into())
					.accept_request(moq_clock::now(), session)
					.await;
				assert!(
					matches!(result, Err(Error::ProtocolViolation)),
					"{alpn}: URI binding accepted a SETUP path"
				);
				assert_eq!(observed.close_code(), Some(3));
			}
		}
	}

	#[moq_clock::test]
	async fn uri_binding_accepts_setup_without_path() {
		let mut session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		session.binding = crate::transport::Binding::Uri;
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[moq_clock::test]
	async fn accept_request_reads_lite05_role() {
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_setup(Some("/team/room"), Some(Role::Publisher), None)],
		);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.role(), Some(Role::Publisher));
	}

	#[moq_clock::test]
	async fn accept_request_holds_uni_stream_before_setup() {
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_group(), lite05_setup(Some("/team/room"), None, None)],
		);
		let stops = session.stops.clone();
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert!(stops.lock().unwrap().is_empty(), "the early stream must stay open");
	}

	#[moq_clock::test]
	async fn accept_request_reads_buffered_setup_after_transport_close() {
		let mut session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/closed"), None, None)]);
		crate::transport::poll::Session::close(&mut session, SessionError::Cancel.to_code(), "closed");
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.path(), "/closed");
	}

	#[moq_clock::test]
	async fn accepted_lite_setup_refuses_a_second_setup_stream() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let transport = session.clone();
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		let (_session, mut driver) = request.ok().await.unwrap();
		transport.uni.lock().unwrap().push_back(vec![1]);
		let _ = driver.poll(moq_clock::now(), &kio::Waiter::noop());
		assert_eq!(transport.close_code(), Some(SessionError::ProtocolViolation.to_code()));
	}

	#[moq_clock::test]
	async fn accept_request_reads_lite05_peer_hop() {
		let hop = Hop::new(42).unwrap();
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, Some(hop))]);
		let request = Server::new().accept_request(moq_clock::now(), session).await.unwrap();
		assert_eq!(request.peer_hop(), Some(hop));
	}

	#[moq_clock::test]
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
			assigned_hop: Hop::random(),
			auth: crate::auth::Handle::ietf(false),
			inner: Some(RequestInner {
				server: Server::new().with_publisher(&origin),
				runtime: Deadlines::new(moq_clock::now()),
				handshake: PausedHandshake::IetfModern(PausedIetfModern {
					session: Transport::new(transport),
					version,
					peer_setup: ietf::PeerSetup {
						stream: crate::coding::Reader::new(
							crate::transport::boxed::RecvStream::new(crate::lite::test_transport::PendingRecv),
							Version::Ietf(version),
						),
						size: 0,
						path: None,
						token: None,
						declared: ietf::peer::Peer::default(),
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
		moq_clock::sim::spawn(crate::time::run(driver));

		for _ in 0..100 {
			if occurrences(&log, b"local-route") > 0 {
				break;
			}
			moq_clock::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert_eq!(occurrences(&log, b"echoed-route"), 0);
		assert_eq!(occurrences(&log, b"local-route"), 1);
		drop(session);
	}
}
