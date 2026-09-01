use crate::origin;
use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_20, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05,
	ALPN_LITE_06_WIP, Consume, Error, NEGOTIATED, Role, Session, SessionError, Version, Versions,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup, stats,
};

// The transport methods are called on the projected `R::Transport`, which needs the
// trait itself in scope (a plain type parameter would not).
use web_transport_trait::{MaybeSend, MaybeSync, poll::Session as _};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Session,
	versions: Versions,
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

	/// Restrict which protocol versions to accept, in preference order.
	/// Defaults to every version this crate supports.
	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
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
	/// with our SETUP, and hand the machine to the runtime.
	fn start_lite<R>(
		&self,
		runtime: R,
		session: R::Transport,
		version: lite::Version,
		client_setup: Option<lite::Setup>,
		peer_hop: Option<crate::Hop>,
	) -> Result<Session, Error>
	where
		R: crate::runtime::Runtime + 'static,
	{
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
			}
		} else {
			lite::Setup::default()
		};

		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			session: session.clone(),
			setup_stream: None,
			publish,
			subscribe,
			peer_hop,
			version,
			our_setup,
			peer_setup: client_setup,
		})?;

		Ok(Session::spawn(
			runtime,
			session,
			version.into(),
			start.recv_bandwidth,
			crate::runtime::Protocol::Lite(Box::new(start.driver)),
			start.goaway,
		))
	}

	/// Perform the MoQ handshake for moq-lite only, over any transport.
	///
	/// Same trade as [`Client::connect_lite`](crate::Client::connect_lite): no
	/// thread-affinity bound on the transport, so a pinned `!Send` transport
	/// works, and only a moq-lite ALPN is accepted (anything else is refused
	/// with [`Error::Version`]). Completes the handshake immediately; a caller
	/// gating on the advertised path uses
	/// [`accept_request_lite`](Self::accept_request_lite) instead.
	pub async fn accept_lite<R>(&self, runtime: R, session: R::Transport) -> Result<Session, Error>
	where
		R: crate::runtime::Runtime + 'static,
	{
		self.accept_request_lite(runtime, session).await?.ok().await
	}

	/// Begin the moq-lite handshake, pausing like
	/// [`accept_request`](Self::accept_request) but for moq-lite ALPNs only,
	/// which is what drops the thread-affinity bounds: a pinned `!Send`
	/// transport can gate on the advertised path too. Anything but a moq-lite
	/// ALPN is refused with [`Error::Version`].
	pub async fn accept_request_lite<R>(
		&self,
		runtime: R,
		mut session: R::Transport,
	) -> Result<Request<R::Transport, R>, Error>
	where
		R: crate::runtime::Runtime + 'static,
	{
		let (path, role, origin, handshake) = match session.protocol() {
			Some(alpn @ (ALPN_LITE_05 | ALPN_LITE_06_WIP)) => {
				let version = match alpn {
					ALPN_LITE_06_WIP => lite::Version::Lite06Wip,
					_ => lite::Version::Lite05,
				};
				self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
				// Gate on the client's SETUP: read it before serving so the
				// caller can scope by the advertised path. Seeded back into
				// `start` on `ok()` so PROBE gating resolves without
				// re-reading the (consumed) Setup Stream.
				let client_setup = lite::accept_setup(&mut session, version).await?;
				(
					client_setup.path.clone(),
					client_setup.role,
					client_setup.hop,
					Handshake::LiteSetup {
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
					Handshake::LiteBare {
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
					Handshake::LiteBare {
						session,
						version: lite::Version::Lite03,
					},
				)
			}
			_ => return Err(Error::Version),
		};

		Ok(Request {
			path,
			role,
			origin,
			assigned_hop: crate::Hop::random(),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake,
			}),
		})
	}

	/// Perform the MoQ handshake as a server, returning the [`Session`].
	///
	/// The session's protocol machine is handed to `runtime`
	/// ([`Runtime::spawn`](crate::runtime::Runtime::spawn)), so there is nothing
	/// else to drive.
	///
	/// Convenience wrapper over [`accept_request`](Self::accept_request) that
	/// completes the handshake immediately. Use `accept_request` when you need to
	/// inspect the client's advertised path before deciding what to serve.
	pub async fn accept<R>(&self, runtime: R, session: R::Transport) -> Result<Session, Error>
	where
		R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
		R::Transport: crate::transport::poll::Boxable,
		<R::Transport as web_transport_trait::poll::Session>::SendStream: MaybeSync,
		<R::Transport as web_transport_trait::poll::Session>::RecvStream: MaybeSync,
		R::Timer: MaybeSend,
	{
		self.accept_request(runtime, session).await?.ok().await
	}

	/// Begin the MoQ handshake, pausing once the client's request path is known so
	/// the caller can authorize/scope before serving.
	///
	/// Reads the client's SETUP (the in-band path lives there on URL-less transports),
	/// then returns a [`Request`]: inspect [`path`](Request::path), set the origins to
	/// serve, and call [`ok`](Request::ok) or [`close`](Request::close). Session start
	/// is deferred to `ok()`, so origins set on the `Request` always take effect.
	///
	/// The path is surfaced for moq-lite-05 and every moq-transport draft we speak;
	/// it's empty on versions with no in-band request path (e.g. lite 01-04).
	pub async fn accept_request<R>(
		&self,
		runtime: R,
		mut session: R::Transport,
	) -> Result<Request<R::Transport, R>, Error>
	where
		R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
		R::Transport: crate::transport::poll::Boxable,
		<R::Transport as web_transport_trait::poll::Session>::SendStream: MaybeSync,
		<R::Transport as web_transport_trait::poll::Session>::RecvStream: MaybeSync,
		R::Timer: MaybeSend,
	{
		let (encoding, supported) = match session.protocol() {
			Some(ALPN_20) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft20))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(runtime, session, ietf::Version::Draft20).await;
			}
			Some(ALPN_19) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft19))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(runtime, session, ietf::Version::Draft19).await;
			}
			Some(ALPN_18) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft18))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(runtime, session, ietf::Version::Draft18).await;
			}
			Some(ALPN_17) => {
				self.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;
				return self.accept_ietf_modern(runtime, session, ietf::Version::Draft17).await;
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
			// Every lite ALPN goes through the same entry point, which is also
			// what a `!Send` transport calls directly.
			Some(ALPN_LITE_05 | ALPN_LITE_06_WIP | ALPN_LITE_04 | ALPN_LITE_03) => {
				return self.accept_request_lite(runtime, session).await;
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		// Legacy bidi SETUP exchange (IETF 14-16, lite 01/02). Read the client's
		// SETUP to choose the version; `ok()` sends the server SETUP and starts.
		let mut stream = Stream::accept(&mut session, encoding).await?;
		let mut client: setup::Client = stream.reader.decode().await?;

		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;

		// Pull the request path and max request ID out now (IETF only) so `ok()`
		// doesn't re-decode the consumed parameters. moq-transport carries the path
		// in its SETUP just like lite-05.
		let (path, request_id_max, peer_declared) = match version {
			Version::Ietf(v) => {
				let params = ietf::Parameters::decode(&mut client.parameters, v)?;
				let path = match params.get_bytes(ietf::ParameterBytes::Path) {
					Some(bytes) => Some(
						std::str::from_utf8(bytes)
							.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
							.to_owned(),
					),
					None => None,
				};
				let request_id_max = params
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);
				let peer_declared = ietf::peer::Peer {
					solicit: ietf::solicit::from_setup(&params, v)?,
					..Default::default()
				};
				(path, request_id_max, peer_declared)
			}
			Version::Lite(_) => (None, None, ietf::peer::Peer::default()),
		};

		Ok(Request {
			path,
			role: None,
			origin: None,
			assigned_hop: crate::Hop::random(),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake: Handshake::Boxed(Box::new(PausedLegacy {
					session,
					stream,
					version,
					request_id_max,
					peer_declared,
				})),
			}),
		})
	}

	/// Read a draft-17/18 client's SETUP (with its request path) off its uni stream,
	/// then pause. `ok()` starts the session and hands the stream back for GOAWAY.
	async fn accept_ietf_modern<R>(
		&self,
		runtime: R,
		mut session: R::Transport,
		version: ietf::Version,
	) -> Result<Request<R::Transport, R>, Error>
	where
		R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
		R::Transport: crate::transport::poll::Boxable,
		<R::Transport as web_transport_trait::poll::Session>::SendStream: MaybeSync,
		<R::Transport as web_transport_trait::poll::Session>::RecvStream: MaybeSync,
		R::Timer: MaybeSend,
	{
		let peer_setup = ietf::accept_setup(&mut session, version).await?;
		Ok(Request {
			path: peer_setup.path.clone(),
			role: None,
			// A moq-transport peer only has an identity if it negotiated the MoQ
			// Cluster extension and declared a non-zero Hop ID.
			origin: peer_setup.declared.cluster.hop.filter(|h| *h != crate::Hop::UNKNOWN),
			assigned_hop: crate::Hop::random(),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake: Handshake::Boxed(Box::new(PausedIetfModern {
					session,
					version,
					peer_setup,
				})),
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
pub struct Request<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	path: Option<String>,
	role: Option<Role>,
	origin: Option<crate::Hop>,
	/// The identity this session's routes are stamped with when the peer declares none
	/// on the wire. Fresh per request unless the caller overrides it
	/// ([`Request::with_peer_hop`]).
	assigned_hop: crate::Hop,
	// Taken by `ok`/`close`; `Drop` rejects the handshake if neither ran.
	inner: Option<RequestInner<S, R>>,
}

/// The parts of a [`Request`] consumed by [`Request::ok`] / [`Request::close`].
struct RequestInner<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	server: Server,
	/// Receives the session's machine once `ok()` completes the handshake.
	runtime: R,
	handshake: Handshake<S, R>,
}

/// The handshake state captured at the pause point. Every variant defers its
/// session start to [`Request::ok`] so origins set on the Request still apply.
enum Handshake<S: crate::transport::poll::Session, R: crate::runtime::Runtime> {
	/// moq-lite 03/04: no Setup Stream.
	LiteBare { session: S, version: lite::Version },
	/// moq-lite 05+: the client's Setup Stream has been read. `ok()` starts the
	/// session, seeding the SETUP back so PROBE gating resolves.
	LiteSetup {
		session: S,
		version: lite::Version,
		client_setup: lite::Setup,
	},
	/// An IETF (or legacy bidi-SETUP) handshake, boxed where its
	/// thread-affinity bounds held. The boxing is what keeps [`Request`] and
	/// its lite path free of those bounds: the ietf machinery erases its
	/// futures, which forces a per-target `Send` choice a pinned `!Send`
	/// transport cannot satisfy, so the choice is made here, at construction,
	/// where the caller proved the bounds.
	Boxed(Box<dyn Paused<R>>),
}

/// A paused non-lite handshake. See [`Handshake::Boxed`] for why this is a
/// trait object.
///
/// `MaybeSync` is not decoration: a caller holding a [`Request`] across an
/// await behind `&self` (moq-relay authenticates that way) needs
/// `&Request: Send`, which is `Request: Sync`, which is this.
trait Paused<R: crate::runtime::Runtime>: MaybeSend + MaybeSync {
	/// Complete the handshake with the final server config.
	fn ok(
		self: Box<Self>,
		server: Server,
		runtime: R,
		peer_hop: Option<crate::Hop>,
	) -> crate::util::MaybeSendBox<'static, Result<Session, Error>>;

	/// Reject the handshake, closing the transport with `err`'s wire code.
	fn close(self: Box<Self>, err: Error);
}

/// Modern IETF (17/18): the client's SETUP (with its request path) has been
/// read off its uni stream; `ok` starts the session, handing that stream back
/// for GOAWAY monitoring.
struct PausedIetfModern<S: crate::transport::poll::Session> {
	session: S,
	version: ietf::Version,
	peer_setup: ietf::PeerSetup<S>,
}

impl<S, R> Paused<R> for PausedIetfModern<S>
where
	S: crate::transport::poll::Session + crate::transport::poll::Boxable,
	S::SendStream: MaybeSync,
	S::RecvStream: MaybeSync,
	R: crate::runtime::Runtime<Transport = S> + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	fn ok(
		self: Box<Self>,
		server: Server,
		runtime: R,
		peer_hop: Option<crate::Hop>,
	) -> crate::util::MaybeSendBox<'static, Result<Session, Error>> {
		use crate::util::MaybeBoxedExt as _;
		async move {
			let Self {
				session,
				version,
				peer_setup,
			} = *self;
			let (publish, subscribe) = server.stat_tagged_origins();

			// The client's SETUP was read at the pause; hand the stream back
			// for GOAWAY. A server never advertises a path, hence `None`.
			let (protocol, goaway) = ietf::start(ietf::Config {
				runtime: runtime.clone(),
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
				peer_setup_stream: Some(peer_setup.stream),
				peer_declared: Some(peer_setup.declared),
			})?;
			tracing::debug!(?version, "connected");
			Ok(Session::spawn(
				runtime,
				session,
				version.into(),
				None,
				crate::runtime::Protocol::Ietf(protocol),
				goaway,
			))
		}
		.maybe_boxed()
	}

	fn close(self: Box<Self>, err: Error) {
		let mut session = self.session;
		session.close(SessionError::from(&err).to_code(), &err.to_string());
	}
}

/// Legacy IETF (draft 14-16) and lite 01/02: the client SETUP has been read
/// off the bidi stream (including its request path) but the server SETUP
/// hasn't been sent; `ok` finishes it.
struct PausedLegacy<S: crate::transport::poll::Session> {
	session: S,
	stream: Stream<S, Version>,
	version: Version,
	request_id_max: Option<ietf::RequestId>,
	/// What the client's SETUP declared, for the options `ok` acts on.
	peer_declared: ietf::peer::Peer,
}

impl<S, R> Paused<R> for PausedLegacy<S>
where
	S: crate::transport::poll::Session + crate::transport::poll::Boxable,
	S::SendStream: MaybeSync,
	S::RecvStream: MaybeSync,
	R: crate::runtime::Runtime<Transport = S> + MaybeSend + MaybeSync + 'static,
	R::Timer: MaybeSend,
{
	fn ok(
		self: Box<Self>,
		server: Server,
		runtime: R,
		peer_hop: Option<crate::Hop>,
	) -> crate::util::MaybeSendBox<'static, Result<Session, Error>> {
		use crate::util::MaybeBoxedExt as _;
		async move {
			let Self {
				session,
				mut stream,
				version,
				request_id_max,
				peer_declared,
			} = *self;
			let (publish, subscribe) = server.stat_tagged_origins();

			// Encode parameters using the version-appropriate type.
			let parameters = match version {
				Version::Ietf(v) => {
					let mut parameters = ietf::Parameters::default();
					parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
					parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
					ietf::solicit::into_setup(&mut parameters, v);
					parameters.encode_bytes(v)?
				}
				Version::Lite(v) => lite::Parameters::default().encode_bytes(v)?,
			};

			let server_setup = setup::Server {
				version: version.into(),
				parameters,
			};
			stream.writer.encode(&server_setup).await?;

			let (recv_bw, protocol, goaway) = match version {
				Version::Lite(v) => {
					let stream = stream.with_version(v);
					// Pre-lite-05: no Setup Stream, so nothing to advertise or seed.
					let start = lite::start(lite::Config {
						runtime: runtime.clone(),
						session: session.clone(),
						setup_stream: Some(stream),
						publish,
						subscribe,
						peer_hop,
						version: v,
						our_setup: lite::Setup::default(),
						peer_setup: None,
					})?;
					(
						start.recv_bandwidth,
						crate::runtime::Protocol::Lite(Box::new(start.driver)),
						start.goaway,
					)
				}
				Version::Ietf(v) => {
					let stream = stream.with_version(v);
					// Draft 14-16: path came in the bidi SETUP, no uni SETUP to hand back.
					let (protocol, goaway) = ietf::start(ietf::Config {
						runtime: runtime.clone(),
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
						peer_setup_stream: None,
						peer_declared: Some(peer_declared),
					})?;
					(None, crate::runtime::Protocol::Ietf(protocol), goaway)
				}
			};

			Ok(Session::spawn(runtime, session, version, recv_bw, protocol, goaway))
		}
		.maybe_boxed()
	}

	fn close(self: Box<Self>, err: Error) {
		let mut session = self.session;
		session.close(SessionError::from(&err).to_code(), &err.to_string());
	}
}

impl<S, R> Request<S, R>
where
	S: crate::transport::poll::Session,
	R: crate::runtime::Runtime<Transport = S> + 'static,
{
	/// The request path the client advertised in its SETUP.
	///
	/// Empty when the client advertised none: either it sent an empty path, or the
	/// version carries none in-band (lite 01-04). Those mean the same thing, so the
	/// wire distinction isn't surfaced. Populated for moq-lite-05 and every
	/// moq-transport draft we speak. See the note on [`Server::accept_request`].
	pub fn path(&self) -> &str {
		self.path.as_deref().unwrap_or("")
	}

	/// The single [`Role`] the client advertised in its SETUP, or `None` for a
	/// bidirectional session.
	///
	/// Only moq-lite-05 carries a role, so `None` covers three cases that the wire
	/// doesn't distinguish: an older version, a client that omitted the parameter, and a
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

	fn inner_mut(&mut self) -> &mut RequestInner<S, R> {
		self.inner.as_mut().expect("request already responded")
	}

	/// Accept the session, returning the [`Session`].
	///
	/// The session's protocol machine is handed to the runtime given to
	/// [`Server::accept_request`], so there is nothing else to drive.
	pub async fn ok(mut self) -> Result<Session, Error> {
		let peer_hop = Some(self.assigned_hop);
		let RequestInner {
			server,
			runtime,
			handshake,
		} = self.inner.take().expect("request already responded");

		match handshake {
			Handshake::LiteBare { session, version } => server.start_lite(runtime, session, version, None, peer_hop),
			Handshake::LiteSetup {
				session,
				version,
				client_setup,
			} => server.start_lite(runtime, session, version, Some(client_setup), peer_hop),
			Handshake::Boxed(paused) => paused.ok(server, runtime, peer_hop).await,
		}
	}

	/// Reject the session, closing the transport with `err`'s wire code.
	pub fn close(mut self, err: Error) {
		let inner = self.inner.take().expect("request already responded");
		inner.close(err);
	}
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> RequestInner<S, R> {
	fn close(self, err: Error) {
		let mut session = match self.handshake {
			Handshake::LiteBare { session, .. } => session,
			Handshake::LiteSetup { session, .. } => session,
			Handshake::Boxed(paused) => return paused.close(err),
		};
		session.close(SessionError::from(&err).to_code(), &err.to_string());
	}
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Runtime> Drop for Request<S, R> {
	// A dropped request would otherwise leave the client hanging until its idle
	// timeout: it already sent SETUP and is waiting on a response. Reject loudly.
	fn drop(&mut self) {
		if let Some(inner) = self.inner.take() {
			tracing::warn!("Request dropped without ok() or close(); rejecting the session");
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
	impl web_transport_trait::Error for FakeError {
		fn session_error(&self) -> Option<(u32, String)> {
			Some((0, "closed".to_string()))
		}
	}

	/// A session that replays a queue of unidirectional streams (each a `Vec<u8>`) in
	/// order from `accept_uni`; everything else is inert.
	#[derive(Clone)]
	struct FakeSession {
		protocol: Option<&'static str>,
		uni: Arc<Mutex<VecDeque<Vec<u8>>>>,
	}

	impl FakeSession {
		fn new(protocol: &'static str, uni: impl IntoIterator<Item = Vec<u8>>) -> Self {
			Self {
				protocol: Some(protocol),
				uni: Arc::new(Mutex::new(uni.into_iter().collect())),
			}
		}
	}

	impl web_transport_trait::poll::Session for FakeSession {
		type SendStream = FakeSend;
		type RecvStream = FakeRecv;
		type Error = FakeError;

		fn poll_accept_uni(
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<Self::RecvStream, Self::Error>> {
			match self.uni.lock().unwrap().pop_front() {
				Some(data) => std::task::Poll::Ready(Ok(FakeRecv { data: data.into() })),
				None => std::task::Poll::Pending,
			}
		}
		fn poll_accept_bi(
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			std::task::Poll::Pending
		}
		fn poll_open_bi(
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			std::task::Poll::Pending
		}
		fn poll_open_uni(
			&mut self,
			_cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
			std::task::Poll::Pending
		}
		fn poll_send_datagram(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			_payload: &[u8],
		) -> std::task::Poll<Result<(), Self::Error>> {
			std::task::Poll::Ready(Ok(()))
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
		fn protocol(&self) -> Option<&str> {
			self.protocol
		}
		fn close(&mut self, _code: u32, _reason: &str) {}
		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Error> {
			std::task::Poll::Pending
		}
		fn stats(&self) -> impl web_transport_trait::Stats {
			web_transport_trait::StatsUnavailable
		}
	}

	#[derive(Clone, Default)]
	struct FakeSend;
	impl web_transport_trait::poll::SendStream for FakeSend {
		type Error = FakeError;
		fn poll_write(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			buf: &[u8],
		) -> std::task::Poll<Result<usize, Self::Error>> {
			std::task::Poll::Ready(Ok(buf.len()))
		}
		fn set_priority(&mut self, _order: u8) {}
		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
		fn reset(&mut self, _code: u32) {}
		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
			std::task::Poll::Ready(Ok(()))
		}
	}

	struct FakeRecv {
		data: VecDeque<u8>,
	}
	impl web_transport_trait::poll::RecvStream for FakeRecv {
		type Error = FakeError;
		fn poll_read(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			dst: &mut [u8],
		) -> std::task::Poll<Result<Option<usize>, Self::Error>> {
			if self.data.is_empty() {
				return std::task::Poll::Ready(Ok(None));
			}
			let size = dst.len().min(self.data.len());
			for slot in dst.iter_mut().take(size) {
				*slot = self.data.pop_front().unwrap();
			}
			std::task::Poll::Ready(Ok(Some(size)))
		}
		fn stop(&mut self, _code: u32) {}
		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
			std::task::Poll::Ready(Ok(()))
		}
	}

	/// Encode a lite-05 Setup Stream: the `DataType::Setup` tag then the SETUP message.
	fn lite05_setup(path: Option<&str>, role: Option<Role>, hop: Option<Hop>) -> Vec<u8> {
		let v = lite::Version::Lite05;
		let mut buf = Vec::new();
		lite::DataType::Setup.encode(&mut buf, v).unwrap();
		lite::Setup {
			probe: lite::ProbeLevel::None,
			path: path.map(str::to_string),
			role,
			cost: None,
			hop,
		}
		.encode(&mut buf, v)
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
		let parameters = params.encode_bytes(version).unwrap();

		let mut buf = Vec::new();
		setup::Setup { parameters }
			.encode(&mut buf, crate::Version::Ietf(version))
			.unwrap();
		buf
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_ietf_path() {
		// Every draft-17+ version gates on the SETUP stream before starting, so the
		// path is known at authorization time just like lite-05.
		for (alpn, version) in [
			(ALPN_17, ietf::Version::Draft17),
			(ALPN_18, ietf::Version::Draft18),
			(ALPN_19, ietf::Version::Draft19),
		] {
			let session = FakeSession::new(alpn, [ietf_setup(version, Some("/team/room"))]);
			let request = Server::new()
				.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
				.await
				.unwrap();
			assert_eq!(request.path(), "/team/room", "{alpn}");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_ietf_without_path_is_empty() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, None)]);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_ietf_empty_path_is_accepted() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, Some(""))]);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.path(), "");
	}

	/// Encode a lite-05 GROUP uni stream header (just the `DataType::Group` tag).
	fn lite05_group() -> Vec<u8> {
		let mut buf = Vec::new();
		lite::DataType::Group.encode(&mut buf, lite::Version::Lite05).unwrap();
		buf
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_path() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/team/room"), None, None)]);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert_eq!(request.role(), None, "a client that omits the role is bidirectional");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_without_path_is_empty() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_empty_path_is_accepted() {
		// An empty path is valid on the wire and means the same as omitting it, so a
		// client that wants the root doesn't have to special-case the parameter.
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some(""), None, None)]);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_role() {
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_setup(Some("/team/room"), Some(Role::Publisher), None)],
		);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.role(), Some(Role::Publisher));
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_skips_uni_stream_before_setup() {
		// A GROUP racing ahead of the SETUP is STOP_SENDING-ed and skipped; the gate
		// keeps reading until it finds the SETUP.
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_group(), lite05_setup(Some("/team/room"), None, None)],
		);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.path(), "/team/room");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_peer_hop() {
		let hop = Hop::new(42).unwrap();
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, Some(hop))]);
		let request = Server::new()
			.accept_request(crate::runtime::tokio_test::Tokio::new(), session)
			.await
			.unwrap();
		assert_eq!(request.peer_hop(), Some(hop));
	}

	#[tokio::test(start_paused = true)]
	async fn anonymous_peer_hop_filters_routes_from_server_session() {
		let other = Hop::new(778).unwrap();
		let origin = crate::origin::Info::new(Hop::new(1).unwrap()).produce();

		let gate = kio::Producer::new(true);
		let transport = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = transport.log.clone();
		let version = ietf::Version::Draft18;
		let request = Request {
			path: None,
			role: None,
			origin: None,
			assigned_hop: Hop::random(),
			inner: Some(RequestInner {
				server: Server::new().with_publisher(&origin),
				runtime: crate::runtime::tokio_test::Tokio::new(),
				handshake: Handshake::Boxed(Box::new(PausedIetfModern {
					session: transport,
					version,
					peer_setup: ietf::PeerSetup {
						stream: crate::coding::Reader::new(
							crate::lite::test_transport::PendingRecv,
							Version::Ietf(version),
						),
						path: None,
						declared: ietf::peer::Peer::default(),
					},
				})),
			}),
		};
		let assigned = request.assigned_hop;

		let mut echoed_hops = crate::Hops::new();
		echoed_hops.push(assigned).unwrap();
		let _echoed = origin
			.announce("echoed-route", crate::origin::Route::default().with_hops(echoed_hops))
			.unwrap();

		let mut local_hops = crate::Hops::new();
		local_hops.push(other).unwrap();
		let _local = origin
			.announce("local-route", crate::origin::Route::default().with_hops(local_hops))
			.unwrap();

		let session = request.ok().await.unwrap();

		for _ in 0..100 {
			if occurrences(&log, b"local-route") > 0 {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert_eq!(occurrences(&log, b"echoed-route"), 0);
		assert_eq!(occurrences(&log, b"local-route"), 1);
		drop(session);
	}
}
