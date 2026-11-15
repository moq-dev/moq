//! Accepting a MoQ session, including the paused handshake that inspects the
//! peer's SETUP before granting origins.

use std::num::NonZeroU64;
use std::task::{Poll, ready};
use std::time::Duration;

use crate::driver::Transport;
use crate::origin;
use crate::time::{Deadlines, Instant};
use crate::transport::poll::Session as _;
use crate::{
	ALPN_18, ALPN_20, ALPN_22, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05, ALPN_LITE_06, ALPN_LITE_07_WIP,
	Consume, Error, NEGOTIATED, Session, SessionError, Version, Versions,
	coding::{Closing, Encode, Reader, Stream, Writer},
	ietf, lite,
	setup::{self, Role},
	stats,
};

/// Tracks admitted connections until their handshake or session ends.
#[derive(Clone, Debug, Default)]
pub struct Admission {
	state: std::sync::Arc<kio::Producer<AdmissionState>>,
}

#[derive(Debug, Default)]
struct AdmissionState {
	active: usize,
	draining: bool,
	closed: Option<Instant>,
}

impl Admission {
	/// Hold the server active until the returned guard is dropped, or `None` once closed.
	pub fn enter(&self) -> Option<Admitted> {
		let mut state = self.state.write().ok()?;
		state.active += 1;
		Some(Admitted {
			state: std::sync::Arc::downgrade(&self.state),
		})
	}

	/// Refuse new admissions as soon as every active connection has left.
	pub fn drain(&self) {
		let Ok(mut state) = self.state.write() else { return };
		if state.draining {
			return;
		}
		state.draining = true;
		if state.active == 0 {
			state.closed = Some(kio::clock::now());
			state.close();
		}
	}

	/// Resolve with the instant admission closed after every active connection left.
	#[kio::wrapper]
	pub fn poll_close(&self, waiter: &kio::Waiter) -> std::task::Poll<Instant> {
		self.drain();
		std::task::ready!(self.state.poll_closed(waiter));
		std::task::Poll::Ready(self.state.read().closed.expect("admission closed"))
	}
}

/// Keeps an admitted connection in the server's active set.
#[derive(Debug)]
#[must_use]
pub struct Admitted {
	// A connection observes its source without keeping it open.
	state: std::sync::Weak<kio::Producer<AdmissionState>>,
}

impl Drop for Admitted {
	fn drop(&mut self) {
		let Some(source) = self.state.upgrade() else { return };
		let Ok(mut state) = source.write() else { return };
		state.active -= 1;
		if state.draining && state.active == 0 {
			state.closed = Some(kio::clock::now());
			state.close();
		}
	}
}

/// How long [`Server`] gives a connection to be accepted, from its arrival through the SETUP
/// reply, unless [`Server::with_timeout`] says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A MoQ server session builder.
#[derive(Clone)]
pub struct Server {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Session,
	versions: Versions,
	limits: crate::session::Limits,
	extensions: setup::Extensions,
	implementation: Option<String>,
	timeout: Option<Duration>,
	receive_window: Option<NonZeroU64>,
}

impl Default for Server {
	fn default() -> Self {
		Self {
			publish: None,
			subscribe: None,
			stats: Default::default(),
			versions: Default::default(),
			limits: Default::default(),
			extensions: Default::default(),
			implementation: None,
			timeout: Some(DEFAULT_TIMEOUT),
			receive_window: None,
		}
	}
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

	/// Bound each accept, from the connection's arrival through SETUP, the caller's admission,
	/// and, on moq-lite 01/02, the server SETUP reply. Other versions send their SETUP from the
	/// started session's driver, outside this bound. A connection that overruns it is closed
	/// with [`Error::Timeout`]'s code. `None` waits forever. Defaults to [`DEFAULT_TIMEOUT`].
	pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
		self.timeout = timeout;
		self
	}

	/// When a connection that arrived at `arrived` must be accepted by, or `None` to wait
	/// forever, including past the clock's range.
	///
	/// A caller that runs a transport handshake before [`accept_request`](Self::accept_request)
	/// bounds that handshake by this instant, then hands it to [`Accept::with_deadline`].
	pub fn deadline(&self, arrived: Instant) -> Option<Instant> {
		arrived.checked_add(self.timeout?)
	}

	/// Open the connection's receive window to `window` once the caller admits the session,
	/// before SETUP is answered, so a transport can hold most of it back until then.
	///
	/// A transport that cannot change its window keeps its own. Unset, the window stays as the
	/// transport set it.
	pub fn with_receive_window(mut self, window: NonZeroU64) -> Self {
		self.receive_window = Some(window);
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
				node: None,
				token: None,
				ietf: self.extensions.ietf && version.has_ietf(),
				implementation: self.implementation(),
				// Filled by `lite::start` from the attached origin handles.
				route_key: None,
				// Filled by `lite::start` from the version.
				lite06_chain: false,
			}
		} else {
			lite::Setup::default()
		};

		// One registry, which the subscriber sends and the session hands out.
		let viewers = crate::bandwidth::Viewers::default();
		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			stats: self.stats.clone(),
			client: false,
			limits: self.limits,
			viewers: viewers.clone(),
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
				peer_target: Some(start.peer_target),
				bandwidth: crate::bandwidth::Allocator::default(),
				viewers,
				peer_implementation: start.peer_implementation,
				datagrams_unrouted: start.datagrams_unrouted,
			},
		))
	}

	/// Pause a lite handshake with the client's SETUP, which only versions with a Setup
	/// Stream carry.
	fn lite_handshake(
		&self,
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
		client_setup: Option<lite::AcceptedSetup>,
	) -> Handshake {
		let (path, role, origin, token, peer_implementation, handshake) = match client_setup {
			// Seeded back into `start` on `ok()` so PROBE gating resolves without
			// re-reading the (consumed) Setup Stream.
			Some(client_setup) => (
				client_setup.setup.path.clone(),
				client_setup.setup.role,
				// Lite-07 declares a Node ID where older versions declare a Hop ID.
				client_setup.setup.hop.or(client_setup.setup.node.map(crate::Hop::from)),
				client_setup.setup.token.clone(),
				client_setup.setup.implementation.clone(),
				PausedHandshake::LiteSetup {
					session,
					version,
					client_setup,
				},
			),
			None => (
				None,
				None,
				None,
				None,
				None,
				PausedHandshake::LiteBare { session, version },
			),
		};
		Handshake {
			path,
			role,
			origin,
			token,
			peer_implementation,
			assigned_hop: crate::Hop::random(),
			timer: Timer::new(None),
			auth: crate::auth::Link::new(version.has_auth()),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake,
			}),
		}
	}

	/// Perform the MoQ handshake as a server, returning the [`Session`] and its [`Driver`](crate::Driver).
	///
	/// Poll the returned driver with nondecreasing time, starting at `now`. The accept
	/// deadline counts from `now` too.
	///
	/// Convenience wrapper over [`accept_request`](Self::accept_request) that
	/// completes the handshake immediately. Use `accept_request` when you need to
	/// inspect the client's advertised path before deciding what to serve.
	pub fn accept(
		&self,
		now: Instant,
		session: impl crate::transport::poll::Boxable,
	) -> impl Future<Output = Result<(Session, crate::Driver), Error>> {
		let mut accept = self.accept_request(now, session);
		let mut answer = None;
		kio::wait(move |waiter| {
			let answer = match &mut answer {
				Some(answer) => answer,
				None => answer.insert(Answer::new(ready!(accept.poll(waiter))?)),
			};
			answer.poll(waiter)
		})
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
	///
	/// The session's clock starts at `now`, and so does the accept deadline
	/// ([`with_timeout`](Self::with_timeout)) unless [`Accept::with_deadline`] moves it. The
	/// deadline goes on to bound the caller's admission ([`Handshake::expired`]) and the SETUP
	/// reply ([`Handshake::ok`]).
	pub fn accept_request(&self, now: Instant, session: impl crate::transport::poll::Boxable) -> Accept {
		Accept {
			server: self.clone(),
			timer: Timer::new(self.deadline(now)),
			// Box before any protocol code runs, so it is compiled once rather than per transport.
			handshake: Handshaking::new(Deadlines::new(now), Transport::new(session)),
			park: kio::Park::default(),
		}
	}

	/// Pause a lite-01/02 handshake once the client's SETUP picked the version; `ok()` sends
	/// the server SETUP and starts.
	fn negotiated_handshake(
		&self,
		runtime: Deadlines,
		session: Transport,
		stream: Stream<Transport, Version>,
		version: lite::Version,
		client_setup_size: usize,
	) -> Handshake {
		Handshake {
			path: None,
			role: None,
			origin: None,
			token: None,
			peer_implementation: None,
			assigned_hop: crate::Hop::random(),
			timer: Timer::new(None),
			// Lite 01/02 carry no AUTH.
			auth: crate::auth::Link::new(false),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake: PausedHandshake::Negotiated(PausedNegotiated {
					session,
					stream,
					version,
					client_setup_size,
				}),
			}),
		}
	}

	/// Pause a moq-transport handshake with the client's SETUP (and its request path).
	/// `ok()` starts the session and hands the SETUP stream back for GOAWAY.
	fn ietf_handshake(
		&self,
		runtime: Deadlines,
		session: Transport,
		version: ietf::Version,
		peer_setup: ietf::PeerSetup,
	) -> Handshake {
		let options = &peer_setup.options;
		Handshake {
			path: options.path.clone(),
			role: None,
			// A moq-transport peer only has an identity if it negotiated the MoQ
			// Cluster extension and declared a non-zero Hop ID.
			origin: options.declared.cluster.hop.filter(|h| *h != crate::Hop::UNKNOWN),
			token: options.token.clone(),
			peer_implementation: options.implementation.clone(),
			assigned_hop: crate::Hop::random(),
			timer: Timer::new(None),
			// moq-transport carries no AUTH: a token rides the requests that name it.
			auth: crate::auth::Link::ietf(),
			inner: Some(RequestInner {
				server: self.clone(),
				runtime,
				handshake: PausedHandshake::Ietf(PausedIetf {
					session,
					version,
					peer_setup,
				}),
			}),
		}
	}
}

/// A connection being accepted, up to the pause where [`Handshake`] hands the client's SETUP to
/// the caller. Await it, or [`poll`](Self::poll) it.
///
/// A failure closes the transport with the error's code, so the peer learns why instead of
/// seeing a bare disconnect; past the deadline, that is [`Error::Timeout`]. Dropped while
/// pending, it releases what it holds and closes nothing.
#[must_use = "an accept does nothing unless awaited or polled"]
pub struct Accept {
	server: Server,
	timer: Timer,
	handshake: Handshaking,
	park: kio::Park,
}

impl Accept {
	/// Give up at `at` rather than [`Server::with_timeout`] after the accept's start, or never
	/// with `None`: for a connection whose transport handshake started the clock earlier, at
	/// the instant [`Server::deadline`] gave.
	pub fn with_deadline(mut self, at: Option<Instant>) -> Self {
		self.timer = Timer::new(at);
		self
	}

	/// The paused handshake, or the error the transport was closed with.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<Handshake, Error>> {
		Self::resolve(&self.server, &mut self.timer, &mut self.handshake, waiter)
	}

	fn resolve(
		server: &Server,
		timer: &mut Timer,
		handshake: &mut Handshaking,
		waiter: &kio::Waiter,
	) -> Poll<Result<Handshake, Error>> {
		let res = ready!(handshake.poll(server, timer, waiter));
		Poll::Ready(res.map(|mut handshake| {
			// The same deadline bounds admission and the reply.
			handshake.timer = std::mem::replace(timer, Timer::new(None));
			handshake
		}))
	}
}

impl Future for Accept {
	type Output = Result<Handshake, Error>;

	fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
		let Self {
			server,
			timer,
			handshake,
			park,
		} = self.get_mut();
		Self::resolve(server, timer, handshake, park.hold(cx))
	}
}

/// A deadline's timer, armed on its first poll, that never fires without a deadline.
struct Timer {
	at: Option<Instant>,
	sleep: Option<kio::clock::Sleep>,
}

impl Timer {
	fn new(at: Option<Instant>) -> Self {
		Self { at, sleep: None }
	}

	/// Ready once the deadline has passed.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.at else {
			return Poll::Pending;
		};
		let sleep = self.sleep.get_or_insert_with(|| kio::clock::sleep_until(at));
		waiter.poll_future(std::pin::Pin::new(sleep))
	}
}

/// The server's side of a handshake, up to the pause where [`Handshake`] hands the client's
/// SETUP to the caller.
struct Handshaking {
	/// Closed on failure. Released once resolved, so it never outlives the handshake.
	refused: Option<Transport>,
	/// After a hand-off, the negotiation stream's receive half, kept open until the handshake
	/// resolves: dropping it can stop the client's half of that stream.
	held: Option<Reader<crate::transport::boxed::RecvStream, Version>>,
	step: AcceptStep,
}

/// Where [`Handshaking`] waits. Every step but `Done` holds the session's clock and transport,
/// which move into the [`Handshake`].
enum AcceptStep {
	/// Not polled yet: the transport's ALPN picks the path.
	Start { runtime: Deadlines, session: Transport },
	/// Reading a moq-transport client's SETUP, holding the uni streams that beat it.
	Ietf {
		runtime: Deadlines,
		session: Transport,
		version: ietf::Version,
		setup: ietf::SetupAccept,
	},
	/// Reading a lite client's Setup Stream, holding the uni streams that beat it, or, with
	/// `None`, a version that has none and pauses at once.
	Lite {
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
		setup: Option<lite::SetupAccept>,
	},
	/// Waiting for the bidi stream a client without a versioned ALPN negotiates on, then
	/// reading its SETUP off it. The SETUP keeps lite-01's framing whichever version it offers.
	Negotiate {
		runtime: Deadlines,
		session: Transport,
		supported: Versions,
		stream: Option<Stream<Transport, Version>>,
	},
	/// Flushing the server SETUP that hands the session off to the picked version's ALPN path.
	HandOff {
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
		writer: Writer<crate::transport::boxed::SendStream, Version>,
	},
	/// Waiting for the client to acknowledge that reply, so a reset cannot discard it.
	HandOffClose {
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
		closing: Closing<crate::transport::boxed::SendStream, Version>,
	},
	/// Resolved.
	Done,
}

impl Handshaking {
	fn new(runtime: Deadlines, session: Transport) -> Self {
		Self {
			refused: Some(session.clone()),
			held: None,
			step: AcceptStep::Start { runtime, session },
		}
	}

	/// The paused handshake, or the error the transport was closed with.
	fn poll(&mut self, server: &Server, timer: &mut Timer, waiter: &kio::Waiter) -> Poll<Result<Handshake, Error>> {
		let res = match self.poll_step(server, waiter) {
			Poll::Ready(res) => res,
			// Polled second, so a SETUP that is ready wins even past the deadline.
			Poll::Pending => {
				ready!(timer.poll(waiter));
				Err(Error::Timeout)
			}
		};
		// Release everything the handshake held before closing over a failure.
		self.step = AcceptStep::Done;
		self.held = None;
		let refused = self.refused.take();
		if let (Err(err), Some(mut refused)) = (&res, refused) {
			close(&mut refused, err, None);
		}
		Poll::Ready(res)
	}

	fn take(&mut self) -> AcceptStep {
		std::mem::replace(&mut self.step, AcceptStep::Done)
	}

	fn poll_step(&mut self, server: &Server, waiter: &kio::Waiter) -> Poll<Result<Handshake, Error>> {
		loop {
			match &mut self.step {
				AcceptStep::Start { .. } => {
					let AcceptStep::Start { runtime, session } = self.take() else {
						unreachable!("matched above");
					};
					self.step = match session.protocol() {
						Some(alpn @ (ALPN_22 | ALPN_20 | ALPN_18)) => {
							let version = match alpn {
								ALPN_22 => ietf::Version::Draft22,
								ALPN_20 => ietf::Version::Draft20,
								_ => ietf::Version::Draft18,
							};
							server.versions.select(Version::Ietf(version)).ok_or(Error::Version)?;
							AcceptStep::Ietf {
								runtime,
								session,
								version,
								setup: ietf::SetupAccept::new(version),
							}
						}
						Some(alpn @ (ALPN_LITE_07_WIP | ALPN_LITE_06 | ALPN_LITE_05 | ALPN_LITE_04 | ALPN_LITE_03)) => {
							let version = match alpn {
								ALPN_LITE_07_WIP => lite::Version::Lite07,
								ALPN_LITE_06 => lite::Version::Lite06,
								ALPN_LITE_05 => lite::Version::Lite05,
								ALPN_LITE_04 => lite::Version::Lite04,
								_ => lite::Version::Lite03,
							};
							Self::lite_step(server, runtime, session, version)?
						}
						// No ALPN, or `moql`: negotiate moq-lite in a bidi SETUP.
						Some(ALPN_LITE) | None => AcceptStep::Negotiate {
							runtime,
							session,
							supported: server.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?,
							stream: None,
						},
						Some(p) => return Poll::Ready(Err(Error::UnknownAlpn(p.to_string()))),
					};
				}
				AcceptStep::Ietf { session, setup, .. } => {
					let peer_setup = ready!(setup.poll(session, waiter))?;
					let AcceptStep::Ietf {
						runtime,
						session,
						version,
						..
					} = self.take()
					else {
						unreachable!("matched above");
					};
					return Poll::Ready(Ok(server.ietf_handshake(runtime, session, version, peer_setup)));
				}
				AcceptStep::Lite { session, setup, .. } => {
					// Gate on the client's SETUP: read it before serving so the caller can
					// scope by the advertised path.
					let client_setup = match setup {
						Some(setup) => Some(ready!(setup.poll(session, waiter))?),
						None => None,
					};
					let AcceptStep::Lite {
						runtime,
						session,
						version,
						..
					} = self.take()
					else {
						unreachable!("matched above");
					};
					return Poll::Ready(Ok(server.lite_handshake(runtime, session, version, client_setup)));
				}
				AcceptStep::Negotiate {
					session,
					supported,
					stream,
					..
				} => {
					let stream = match stream {
						Some(stream) => stream,
						None => stream.insert(ready!(Stream::poll_accept(
							session,
							Version::Lite(lite::Version::Lite01),
							waiter
						))?),
					};
					// Stats attach in `ok()`: a caller may still replace them on the handshake.
					let (client, client_setup_size) = ready!(stream.reader.poll_decode_sized::<setup::Client>(waiter))?;
					let version = client.select(supported)?;
					let AcceptStep::Negotiate {
						runtime,
						session,
						stream: Some(stream),
						..
					} = self.take()
					else {
						unreachable!("matched above");
					};
					if !Version::Lite(version).hands_off() {
						return Poll::Ready(Ok(server.negotiated_handshake(
							runtime,
							session,
							stream,
							version,
							client_setup_size,
						)));
					}

					// Answer, close the stream, and continue as that version's ALPN would.
					let reply = setup::Server {
						version: Version::Lite(version).into(),
						parameters: lite::Parameters::default().encode_bytes(version)?,
					};
					let Stream { mut writer, reader } = stream;
					self.held = Some(reader);
					writer.buffer(&reply)?;
					self.step = AcceptStep::HandOff {
						runtime,
						session,
						version,
						writer,
					};
				}
				AcceptStep::HandOff { writer, .. } => {
					ready!(writer.poll_flush(waiter))?;
					let AcceptStep::HandOff {
						runtime,
						session,
						version,
						writer,
					} = self.take()
					else {
						unreachable!("matched above");
					};
					self.step = AcceptStep::HandOffClose {
						runtime,
						session,
						version,
						closing: writer.closing()?,
					};
				}
				AcceptStep::HandOffClose { closing, .. } => {
					ready!(closing.poll(waiter))?;
					let AcceptStep::HandOffClose {
						runtime,
						session,
						version,
						..
					} = self.take()
					else {
						unreachable!("matched above");
					};
					self.step = Self::lite_step(server, runtime, session, version)?;
				}
				AcceptStep::Done => panic!("handshake polled after it resolved"),
			}
		}
	}

	/// The step that reads a lite client's SETUP, refusing a version the server doesn't speak.
	fn lite_step(
		server: &Server,
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
	) -> Result<AcceptStep, Error> {
		server.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
		Ok(AcceptStep::Lite {
			runtime,
			session,
			version,
			setup: version.has_setup_stream().then(|| lite::SetupAccept::new(version)),
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
	/// The accept deadline, which bounds admission and the reply.
	timer: Timer,
	// Taken by `ok`/`close`, or by the deadline; `Drop` rejects the handshake if none ran.
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
		let (protocol, goaway) = ietf::start(ietf::Config {
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
				credential: None,
				estimate: None,
				peer_target: None,
				bandwidth: crate::bandwidth::Allocator::default(),
				viewers: Default::default(),
				peer_implementation,
				datagrams_unrouted: None,
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
	/// Buffer the server SETUP, counting the client's under the final stats. The returned
	/// reply sends it and starts the session.
	fn reply(
		self,
		server: Server,
		runtime: Deadlines,
		peer_hop: Option<crate::Hop>,
		auth: crate::auth::Link,
	) -> Result<NegotiatedReply, Error> {
		let Self {
			session,
			stream,
			version,
			client_setup_size,
		} = self;
		let mut stream = stream.with_stats(&server.stats);
		server.stats.control(stats::Message::Session, client_setup_size, true);
		let (publish, subscribe) = server.stat_tagged_origins();

		stream.writer.buffer(&setup::Server {
			version: Version::Lite(version).into(),
			parameters: lite::Parameters::default().encode_bytes(version)?,
		})?;
		Ok(NegotiatedReply(Some(NegotiatedStart {
			server,
			runtime,
			session,
			stream,
			version,
			peer_hop,
			auth,
			publish,
			subscribe,
		})))
	}
}

/// A negotiated client's server SETUP on its way out. Dropped before it lands, it resets the
/// negotiation stream.
struct NegotiatedReply(Option<NegotiatedStart>);

impl NegotiatedReply {
	/// The started session, once the server SETUP has hit the stream.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(Session, crate::Driver), Error>> {
		let start = self.0.as_mut().expect("reply polled after it resolved");
		let flushed = ready!(start.stream.writer.poll_flush(waiter));
		let start = self.0.take().expect("checked above");
		Poll::Ready(flushed.and_then(|()| start.start()))
	}
}

/// What a negotiated session starts with, held while its server SETUP flushes.
struct NegotiatedStart {
	server: Server,
	runtime: Deadlines,
	session: Transport,
	stream: Stream<Transport, Version>,
	version: lite::Version,
	peer_hop: Option<crate::Hop>,
	auth: crate::auth::Link,
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
}

impl NegotiatedStart {
	fn start(self) -> Result<(Session, crate::Driver), Error> {
		let Self {
			server,
			runtime,
			session,
			stream,
			version,
			peer_hop,
			auth,
			publish,
			subscribe,
		} = self;
		let stream = stream.with_version(version);
		// Pre-lite-05: no Setup Stream, so nothing to advertise or seed.
		// One registry, which the subscriber sends and the session hands out.
		let viewers = crate::bandwidth::Viewers::default();
		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			stats: server.stats.clone(),
			client: false,
			limits: server.limits,
			viewers: viewers.clone(),
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
				peer_target: Some(start.peer_target),
				bandwidth: crate::bandwidth::Allocator::default(),
				viewers,
				peer_implementation: start.peer_implementation,
				datagrams_unrouted: start.datagrams_unrouted,
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

	/// The identity the peer declared for itself, when the negotiated protocol carries one.
	///
	/// A moq-lite-05+ endpoint declares this when it attaches a publish or subscribe
	/// origin: its Hop ID, or from moq-lite-07 its Node ID. A moq-transport endpoint
	/// declares its Hop ID via the MoQ Cluster extension. Older versions and endpoints
	/// without one return `None`. [`origin::Producer::is_own`] tells whether it is ours.
	///
	/// Self-declared, so treat it as a correlation hint rather than an
	/// authenticated identity: authorize on the token or client certificate.
	pub fn peer_hop(&self) -> Option<std::num::NonZeroU64> {
		self.origin.and_then(|hop| std::num::NonZeroU64::new(hop.id()))
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
	pub fn with_publisher(self, publish: impl Consume<origin::Consumer>) -> Self {
		let publish = publish.consume();
		self.configure(|server| server.publish = Some(publish))
	}

	/// Subscribe to the connected client. Overrides any value from the [`Server`] builder.
	pub fn with_subscriber(self, subscribe: origin::Producer) -> Self {
		self.configure(|server| server.subscribe = Some(subscribe))
	}

	/// Assign the hop id this peer's routes are attributed to, overriding the fresh
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
	pub fn with_peer_hop(mut self, hop: std::num::NonZeroU64) -> Self {
		self.assigned_hop = crate::Hop::from(hop);
		self
	}

	/// Set the per-connection [`stats::Session`] context. Overrides any value from the
	/// [`Server`] builder.
	pub fn with_stats(self, stats: stats::Session) -> Self {
		self.configure(|server| server.stats = stats)
	}

	/// Set the session's limits after authorizing the peer and before answering SETUP.
	pub fn with_limits(self, limits: crate::session::Limits) -> Self {
		self.configure(|server| server.limits = limits)
	}

	/// Open the receive window to `window` on [`ok`](Self::ok). Overrides any value from the
	/// [`Server`] builder; see [`Server::with_receive_window`].
	pub fn with_receive_window(self, window: NonZeroU64) -> Self {
		self.configure(|server| server.receive_window = Some(window))
	}

	/// Change the server config the session starts with, unless the deadline already refused it.
	fn configure(mut self, change: impl FnOnce(&mut Server)) -> Self {
		if let Some(inner) = &mut self.inner {
			change(&mut inner.server);
		}
		self
	}

	/// Resolve once the accept deadline passes, having refused the session with
	/// [`Error::Timeout`]; never, without a deadline.
	///
	/// Race the admission decision against it, so a peer cannot hold the connection open
	/// behind a decision that never comes. Once it resolves, [`ok`](Self::ok) fails with the
	/// timeout.
	#[kio::wrapper]
	pub fn poll_expired(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		ready!(self.timer.poll(waiter));
		if let Some(inner) = self.inner.take() {
			inner.close(Error::Timeout);
		}
		Poll::Ready(())
	}

	/// Accept the session, returning the [`Session`] and its [`Driver`](crate::Driver).
	///
	/// Opens the receive window first ([`with_receive_window`](Self::with_receive_window)).
	/// Fails with [`Error::Timeout`] once the accept deadline passes, whether admission or a
	/// moq-lite 01/02 peer that never takes the SETUP reply ran it out. Dropping a pending `ok` refuses the
	/// session the same way, so a caller's own deadline counts too.
	///
	/// Poll or spawn the returned driver to run the session.
	pub async fn ok(self) -> Result<(Session, crate::Driver), Error> {
		let mut answer = Answer::new(self);
		kio::wait(|waiter| answer.poll(waiter)).await
	}

	/// Reject the session, closing the transport with `err`'s wire code. Does nothing once
	/// the deadline refused it.
	pub fn close(mut self, err: Error) {
		if let Some(inner) = self.inner.take() {
			inner.close(err);
		}
	}
}

impl RequestInner {
	fn close(self, err: Error) {
		self.server.stats.end(&err);
		let (mut session, version) = self.handshake.transport();
		close(&mut session, &err, Some(version));
	}
}

/// [`Handshake::ok`] as a poll: starts the session, once a negotiated client has taken the
/// server SETUP. Dropped before it resolves, it refuses the session as the deadline does.
// Fields drop in order: the reply before the guard refuses, and the handshake last.
struct Answer {
	/// What starts the session, taken by the first poll; `None` when the deadline already
	/// refused it.
	start: Option<(Server, Deadlines, PausedHandshake)>,
	/// A negotiated client's server SETUP on its way out. Boxed so the bidi SETUP write of
	/// these old versions doesn't size every caller's handshake.
	reply: Option<Box<NegotiatedReply>>,
	/// The accept deadline.
	timer: Timer,
	unanswered: Unanswered,
	/// The request being answered, emptied of what the start took.
	handshake: Handshake,
}

impl Answer {
	fn new(mut handshake: Handshake) -> Self {
		let timer = std::mem::replace(&mut handshake.timer, Timer::new(None));
		let Some(RequestInner {
			server,
			runtime,
			handshake: paused,
		}) = handshake.inner.take()
		else {
			return Self {
				start: None,
				reply: None,
				timer,
				unanswered: Unanswered(None),
				handshake,
			};
		};
		let stats = server.stats.clone();
		let (session, version) = paused.transport();
		Self {
			start: Some((server, runtime, paused)),
			reply: None,
			timer,
			unanswered: Unanswered(Some((session, version, stats))),
			handshake,
		}
	}

	/// The started session, or why it failed.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(Session, crate::Driver), Error>> {
		let res = ready!(self.poll_reply(waiter));
		// Release the reply before the guard closes over a deadline.
		self.reply = None;
		self.timer = Timer::new(None);
		let Some(res) = res else {
			drop(std::mem::replace(&mut self.unanswered, Unanswered(None)));
			return Poll::Ready(Err(Error::Timeout));
		};
		match &res {
			Ok(_) => self.unanswered.answered(),
			Err(err) => self.unanswered.refuse(err),
		}
		Poll::Ready(res)
	}

	/// The reply's outcome, or `None` once the deadline passed first.
	fn poll_reply(&mut self, waiter: &kio::Waiter) -> Poll<Option<Result<(Session, crate::Driver), Error>>> {
		if let Some((server, runtime, paused)) = self.start.take() {
			// Admission ran out the deadline: refuse rather than start.
			if self.timer.poll(waiter).is_ready() {
				return Poll::Ready(None);
			}
			if let Err(err) = open_window(&server, &paused) {
				return Poll::Ready(Some(Err(err)));
			}
			let peer_hop = Some(self.handshake.assigned_hop);
			let auth = self.handshake.auth.clone();
			let started = match paused {
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
				PausedHandshake::Negotiated(paused) => match paused.reply(server, runtime, peer_hop, auth) {
					Ok(reply) => {
						self.reply = Some(Box::new(reply));
						return self.poll_reply(waiter);
					}
					Err(err) => Err(err),
				},
			};
			return Poll::Ready(Some(started));
		}
		// Neither a start nor a reply: the deadline refused the request before `ok`.
		let Some(reply) = self.reply.as_mut() else {
			return Poll::Ready(None);
		};
		// The reply is polled first, so one that is ready wins even past the deadline.
		if let Poll::Ready(res) = reply.poll(waiter) {
			return Poll::Ready(Some(res));
		}
		ready!(self.timer.poll(waiter));
		Poll::Ready(None)
	}
}

/// Open the receive window the transport held back until admission, before anything answers
/// SETUP. A transport that cannot change it never held it back.
fn open_window(server: &Server, paused: &PausedHandshake) -> Result<(), Error> {
	let Some(window) = server.receive_window else {
		return Ok(());
	};
	let limits = crate::transport::Limits {
		receive_window: Some(window),
	};
	match paused.transport().0.set_limits(limits) {
		Ok(()) | Err(crate::transport::LimitsError::Unsupported) => Ok(()),
		Err(err) => Err(Error::Transport(err.to_string())),
	}
}

/// A SETUP reply the peer has not taken yet. Dropped unanswered, it refuses the session with
/// [`Error::Timeout`]: the reply waits only on the peer, so giving up on it means the peer
/// took too long.
struct Unanswered(Option<(Transport, Version, stats::Session)>);

impl Unanswered {
	fn answered(&mut self) {
		self.0 = None;
	}

	/// Close the session with `err`'s wire code, so the peer learns why it was refused.
	fn refuse(&mut self, err: &Error) {
		if let Some((mut session, version, stats)) = self.0.take() {
			close(&mut session, err, Some(version));
			stats.end(err);
		}
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

	#[kio::test]
	async fn admission_closes_at_its_last_drop() {
		use futures::FutureExt;
		use std::time::Duration;

		let admission = Admission::default();
		let first = admission.enter().unwrap();
		admission.drain();
		let last = admission.enter().expect("admission stays open while active");
		kio::clock::sleep(Duration::from_secs(1)).await;
		drop(first);
		assert!(admission.close().now_or_never().is_none());
		kio::clock::sleep(Duration::from_secs(1)).await;
		let ended = kio::clock::now();
		drop(last);
		assert!(admission.enter().is_none(), "last drop did not close admission");
		kio::clock::sleep(Duration::from_secs(100)).await;
		assert_eq!(admission.close().await, ended, "the observer changed the close time");

		let admission = Admission::default();
		let active = admission.enter().unwrap();
		let source = admission.state.weak();
		drop(admission);
		assert!(source.is_closed(), "an active connection kept its source open");
		drop(active);
	}

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
		/// The receive window `set_limits` opened, or why it refused, or `None` when the
		/// transport cannot change it.
		window: Option<Opened>,
	}

	/// The window a transport opened, or why it refused.
	type Opened = Arc<Mutex<Result<Option<std::num::NonZeroU64>, String>>>;

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
				window: None,
			}
		}

		/// A transport that can change its receive window, or refuses with `refusal`.
		fn widening(mut self, refusal: Option<&str>) -> Self {
			let state = match refusal {
				Some(reason) => Err(reason.to_string()),
				None => Ok(None),
			};
			self.window = Some(Arc::new(Mutex::new(state)));
			self
		}

		/// The receive window `set_limits` opened.
		fn window(&self) -> Option<std::num::NonZeroU64> {
			let window = self.window.as_ref().expect("a widening transport");
			window.lock().unwrap().clone().expect("limits accepted")
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

		fn poll_open_uni(
			&self,
			_waiter: &kio::Waiter,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
			std::task::Poll::Pending
		}
		fn set_limits(&self, limits: crate::transport::Limits) -> Result<(), crate::transport::LimitsError> {
			let Some(window) = &self.window else {
				return Err(crate::transport::LimitsError::Unsupported);
			};
			let mut window = window.lock().unwrap();
			match &mut *window {
				Ok(opened) => {
					*opened = limits.receive_window;
					Ok(())
				}
				Err(reason) => Err(crate::transport::LimitsError::Refused(reason.clone())),
			}
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
		fn export_keying_material(&self, _label: &[u8], _context: &[u8]) -> Option<[u8; 32]> {
			Some([9; 32])
		}
	}

	#[derive(Clone, Default)]
	struct FakeSend {
		withheld: bool,
	}
	impl crate::transport::poll::SendStream for FakeSend {
		type Error = FakeError;
		type Group = crate::transport::poll::Ungrouped;
		type Offset = ();

		fn offset(&self) -> Self::Offset {}
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
			node: None,
			token: None,
			ietf: false,
			implementation: None,
			route_key: None,
			lite06_chain: false,
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
	/// Its auth link routes a request token to that pre-ok consumer after `ok()`, instead of
	/// falling to the `Unsupported` default, and granting admits.
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
			let auth = request.auth.clone();
			let (_net_session, _driver) = request.ok().await.unwrap_or_else(|e| panic!("ok at {version:?}: {e}"));

			// The same link passed to the driver must still reach the pre-ok consumer.
			let verdict = auth.verify_request(
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
		let deadline = start.saturating_add(DEFAULT_TIMEOUT);
		let request = Server::new()
			.accept_request(start, session.clone())
			.await
			.unwrap()
			.with_stats(
				registry
					.tier(Default::default())
					.session(crate::stats::SessionInfo::new(crate::Path::from_static("room"))),
			);
		let Err(err) = request.ok().await else {
			panic!("a reply the peer never took completed");
		};
		assert!(matches!(err, Error::Timeout), "{err:?}");
		assert_eq!(kio::clock::now(), deadline);
		assert_eq!(session.close_code(), Some(version.session_code(&SessionError::Timeout)));
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(stats::Failure::Application), 1, "{failed:?}");
	}

	/// A client that never sends its SETUP is refused at the accept deadline with the timeout
	/// code, which its version can't pick yet. The deadline is the one the caller handed over,
	/// as after a transport handshake that started the clock.
	#[kio::test]
	async fn a_setup_that_never_arrives_fails_at_the_deadline() {
		for session in [
			FakeSession::new(ALPN_22, []),
			FakeSession::new(ALPN_LITE_05, []),
			FakeSession::new(ALPN_LITE, []),
		] {
			let deadline = kio::clock::now().saturating_add(std::time::Duration::from_secs(3));
			let accept = Server::new()
				.accept_request(kio::clock::now(), session.clone())
				.with_deadline(Some(deadline));
			let Err(err) = accept.await else {
				panic!("{:?}: accepted without a SETUP", session.protocol);
			};
			assert!(matches!(err, Error::Timeout), "{err:?}");
			assert_eq!(kio::clock::now(), deadline);
			assert_eq!(session.close_code(), Some(SessionError::Timeout.to_code()));
		}
	}

	/// An admission decision that never comes is refused at the deadline the accept started,
	/// counted in the session's stats, and `ok` after it fails rather than starting.
	#[kio::test]
	async fn an_admission_past_the_deadline_is_refused() {
		let registry = stats::Registry::new(Default::default());
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("room"), None, None)]);
		let start = kio::clock::now();
		let mut request = Server::new()
			.with_timeout(Some(std::time::Duration::from_secs(5)))
			.accept_request(start, session.clone())
			.await
			.unwrap()
			.with_stats(
				registry
					.tier(Default::default())
					.session(crate::stats::SessionInfo::new(crate::Path::from_static("room"))),
			);
		request.expired().await;
		assert_eq!(
			kio::clock::now(),
			start.saturating_add(std::time::Duration::from_secs(5))
		);
		let version = Version::Lite(lite::Version::Lite05);
		assert_eq!(session.close_code(), Some(version.session_code(&SessionError::Timeout)));
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(stats::Failure::Application), 1, "{failed:?}");
		let Err(err) = request.with_limits(Default::default()).ok().await else {
			panic!("an expired handshake started");
		};
		assert!(matches!(err, Error::Timeout), "{err:?}");
	}

	/// A caller that admits past the deadline without racing it gets the timeout from `ok`.
	#[kio::test]
	async fn ok_past_the_deadline_is_refused() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let request = Server::new()
			.with_timeout(Some(std::time::Duration::from_secs(1)))
			.accept_request(kio::clock::now(), session.clone())
			.await
			.unwrap();
		kio::clock::sleep(std::time::Duration::from_secs(1)).await;
		let Err(err) = request.ok().await else {
			panic!("a late admission started");
		};
		assert!(matches!(err, Error::Timeout), "{err:?}");
		let version = Version::Lite(lite::Version::Lite05);
		assert_eq!(session.close_code(), Some(version.session_code(&SessionError::Timeout)));
	}

	/// Admitting opens the receive window the server or the request names, before the
	/// session starts; a transport that cannot change its window starts as it is.
	#[kio::test]
	async fn ok_opens_the_receive_window() {
		let window = std::num::NonZeroU64::new(1 << 20).unwrap();
		let wider = std::num::NonZeroU64::new(4 << 20).unwrap();
		let server = Server::new().with_receive_window(window);

		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]).widening(None);
		let request = server.accept_request(kio::clock::now(), session.clone()).await.unwrap();
		let _accepted = request.ok().await.expect("ok");
		assert_eq!(session.window(), Some(window));

		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]).widening(None);
		let request = server.accept_request(kio::clock::now(), session.clone()).await.unwrap();
		let _accepted = request.with_receive_window(wider).ok().await.expect("ok");
		assert_eq!(session.window(), Some(wider));

		let fixed = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let request = server.accept_request(kio::clock::now(), fixed.clone()).await.unwrap();
		let _accepted = request.ok().await.expect("a transport that can't widen starts");
		assert_eq!(fixed.close_code(), None);
	}

	/// A window the transport refuses fails the session and tells the peer, rather than
	/// serving it on the window it was held back to.
	#[kio::test]
	async fn a_refused_window_closes_the_session() {
		let window = std::num::NonZeroU64::new(1 << 20).unwrap();
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]).widening(Some("too small"));
		let request = Server::new()
			.with_receive_window(window)
			.accept_request(kio::clock::now(), session.clone())
			.await
			.unwrap();
		let Err(err) = request.ok().await else {
			panic!("served on a refused window");
		};
		assert!(matches!(err, Error::Transport(_)), "{err:?}");
		let version = Version::Lite(lite::Version::Lite05);
		let code = version.session_code(&SessionError::from(&err));
		assert_eq!(session.close_code(), Some(code));
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
					.session(crate::stats::SessionInfo::new(crate::Path::from_static("room"))),
			);
		let abandoned = kio::clock::timeout(std::time::Duration::from_secs(1), request.ok()).await;
		assert!(abandoned.is_err(), "a reply the peer never took completed");
		assert_eq!(session.close_code(), Some(version.session_code(&SessionError::Timeout)));
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(stats::Failure::Application), 1, "{failed:?}");
	}

	/// A handshake the caller stops waiting for before the client's SETUP arrives closes
	/// nothing and lets go of the transport, so the caller's own deadline decides the close.
	#[kio::test]
	async fn an_abandoned_accept_closes_nothing() {
		for session in [
			FakeSession::new(ALPN_22, []),
			FakeSession::new(ALPN_LITE_05, []),
			FakeSession::new(ALPN_LITE, []),
		] {
			let observed = session.clone();
			let abandoned = kio::clock::timeout(
				std::time::Duration::from_secs(1),
				Server::new().accept_request(kio::clock::now(), session),
			)
			.await;
			assert!(abandoned.is_err(), "{:?}: accepted without a SETUP", observed.protocol);
			assert_eq!(observed.close_code(), None, "{:?}", observed.protocol);
			assert_eq!(Arc::strong_count(&observed.closed), 1, "{:?}", observed.protocol);
		}
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
						.session(crate::stats::SessionInfo::new(crate::Path::from_static("room"))),
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

	/// A client whose route key proof fails is refused with PROTOCOL_VIOLATION, not left waiting.
	#[kio::test]
	async fn a_failed_route_key_proof_closes_with_its_code() {
		let v = lite::Version::Lite07;
		// Signed for another session's exported secret, so it fails against this one's.
		let proof = crate::route::Transcript::new([1; 32]).prove(&crate::origin::RouteKey::generate(), true);
		let mut setup = Vec::new();
		lite::DataType::Setup
			.encode(&mut crate::coding::Encoder::new(&mut setup, v.into()), v)
			.unwrap();
		lite::Setup {
			route_key: Some(proof),
			..Default::default()
		}
		.encode(&mut crate::coding::Encoder::new(&mut setup, v.into()), v)
		.unwrap();

		let session = FakeSession::new(ALPN_LITE_07_WIP, [setup]);
		let transport = session.clone();
		let request = Server::new()
			.with_versions(Version::Lite(v).into())
			.accept_request(kio::clock::now(), session)
			.await
			.unwrap();
		assert!(matches!(request.ok().await, Err(Error::ProtocolViolation)));
		assert_eq!(
			transport.close_code(),
			Some(Version::Lite(v).session_code(&SessionError::ProtocolViolation))
		);
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
		assert_eq!(request.peer_hop(), Some(std::num::NonZeroU64::new(42).unwrap()));
	}

	#[kio::test]
	async fn anonymous_peer_hop_filters_routes_from_server_session() {
		let other = Hop::new(778).unwrap();
		let origin = crate::origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();

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
			auth: crate::auth::Link::ietf(),
			timer: Timer::new(None),
			inner: Some(RequestInner {
				server: Server::new().with_publisher(&origin),
				runtime: Deadlines::new(kio::clock::now()),
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
				crate::origin::RouteInfo::new()
					.with_hops(echoed_hops)
					.with_via(assigned),
			)
			.unwrap();

		let mut local_hops = crate::Hops::new();
		local_hops.push(other).unwrap();
		let _local = origin
			.announce("local-route", crate::origin::RouteInfo::new().with_hops(local_hops))
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
