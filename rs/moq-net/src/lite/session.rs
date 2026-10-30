use crate::driver::Transport;
use crate::origin;
use crate::transport::poll::Session as _;
use crate::{
	Error, Hop, SessionError, bandwidth,
	coding::{Reader, Stream, Writer},
	lite::SessionInfo,
};

use std::task::{Poll, ready};

use super::{
	DataType, PeerSetup, Publisher, PublisherConfig, Setup, Subscriber, SubscriberConfig, SubscriberDriver, Version,
};

pub(crate) struct SessionStart {
	pub recv_bandwidth: Option<bandwidth::Weak>,
	/// The estimate PROBE reports, published by the session's supervisor.
	pub estimate: kio::Producer<crate::session::Estimate>,
	/// The session's protocol machine over the boxed transport.
	pub driver: Driver,
	/// The session-side GOAWAY halves, stored on the public [`crate::Session`].
	pub goaway: crate::goaway::Handle,
	/// The session's AUTH tokens and grants, stored on the public [`crate::Session`].
	pub auth: crate::auth::Link,
	/// The connection credential presented on `auth`, stored on the public [`crate::Session`].
	pub credential: Option<crate::auth::Credential>,
	/// The implementation the peer's SETUP advertised, stored on the public [`crate::Session`].
	pub peer_implementation: crate::setup::PeerImplementation,
}

/// Server: read the peer's single SETUP message off its Setup Stream before starting
/// the session, so the caller can inspect the advertised path (and gate on it) before
/// serving. lite-05+ only.
///
/// Other uni streams racing ahead of SETUP are held for the normal session
/// classifier. QUIC stream credit bounds the held queue.
///
/// Pass the result to [`start`] as its `peer_setup` so capability gating resolves
/// without re-reading SETUP, and every held stream is handed back to the driver.
pub async fn accept_setup(session: &mut Transport, version: Version) -> Result<AcceptedSetup, Error> {
	let mut accept = SetupAccept {
		version,
		pending: Vec::new(),
		early: Vec::new(),
		setup: None,
	};
	kio::wait(|waiter| accept.poll(session, waiter)).await
}

struct SetupAccept {
	version: Version,
	pending: Vec<Reader<crate::transport::boxed::RecvStream, Version>>,
	early: Vec<Reader<crate::transport::boxed::RecvStream, Version>>,
	setup: Option<Reader<crate::transport::boxed::RecvStream, Version>>,
}

impl SetupAccept {
	fn poll(&mut self, session: &mut Transport, waiter: &kio::Waiter) -> Poll<Result<AcceptedSetup, Error>> {
		let Self {
			version,
			pending,
			early,
			setup,
		} = self;
		let version = *version;

		let mut accept_error = None;
		while let Poll::Ready(stream) = session.poll_accept_uni(waiter) {
			match stream {
				Ok(stream) => pending.push(Reader::new(stream, version)),
				Err(err) => {
					accept_error = Some(crate::lite::error::from_transport(err));
					break;
				}
			}
		}
		let mut index = 0;
		while index < pending.len() {
			let kind = match pending[index].poll_varint_peek(waiter) {
				Poll::Pending => {
					index += 1;
					continue;
				}
				Poll::Ready(Ok(kind)) => kind,
				Poll::Ready(Err(
					Error::Cancel
					| Error::Stream(_)
					| Error::Remote(_)
					| Error::Transport(_)
					| Error::IdleTimeout
					| Error::Decode(crate::DecodeError::Short),
				)) => {
					pending.swap_remove(index);
					continue;
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
			};
			let mut reader = pending.swap_remove(index);
			if kind != DataType::Setup as u64 {
				early.push(reader);
				continue;
			}
			if setup.is_some() {
				return Poll::Ready(Err(Error::ProtocolViolation));
			}
			ready!(reader.poll_decode::<DataType>(waiter))?;
			reader.set_priority(crate::priority::RECV_CONTROL);
			*setup = Some(reader);
		}
		// A closed transport can still yield a complete buffered SETUP. Decode it
		// before reporting the terminal error from accepting the next stream.
		let result = setup
			.as_mut()
			.map(|reader| reader.poll_decode_sized::<Setup>(waiter))
			.unwrap_or(Poll::Pending);
		let (setup, size) = match result {
			Poll::Ready(result) => result?,
			Poll::Pending => match accept_error {
				Some(err) => return Poll::Ready(Err(err)),
				None => return Poll::Pending,
			},
		};
		if session.address() == crate::transport::Address::Uri && setup.path.is_some() {
			return Poll::Ready(Err(Error::ProtocolViolation));
		}
		// A stream need not have delivered even its type yet. Keep it intact for the
		// session instead of letting it block SETUP or dropping it at the handoff.
		early.append(pending);
		Poll::Ready(Ok(AcceptedSetup {
			setup,
			size,
			early: std::mem::take(early),
		}))
	}
}

/// The peer's pre-read SETUP and the streams held until it arrived.
pub struct AcceptedSetup {
	/// The SETUP decoded before origin binding.
	pub setup: Setup,
	/// The SETUP's encoded size, counted once the session's stats are known.
	pub size: usize,
	/// Streams with their type still buffered for the session classifier.
	pub early: Vec<Reader<crate::transport::boxed::RecvStream, Version>>,
}

/// Everything one moq-lite session needs to start.
pub struct Config {
	/// The runtime that arms the session's timers.
	pub runtime: crate::time::Deadlines,

	/// Where the session's control traffic is counted, even with neither origin set.
	pub stats: crate::stats::Session,

	/// Whether we dialed the session. Only the dialing side aborts on a publication
	/// its grant does not cover: a server's publish origin is everything the peer
	/// may read, not what it intends to push.
	pub client: bool,

	/// The transport carrying the session. Cloned into every loop that outlives
	/// [`start`], so the connection closes when the last of them drops.
	pub session: Transport,

	/// The stream used to set up the session, after exchanging setup messages.
	/// NOTE: No longer used in draft-03.
	pub setup_stream: Option<Stream<Transport, Version>>,

	/// We will publish any local broadcasts from this origin, when set.
	pub publish: Option<origin::Consumer>,

	/// We will consume any remote broadcasts, inserting them into this origin, when
	/// set. Traffic stats are attributed through these origin handles: tag them with
	/// `origin::{Consumer, Producer}::with_stats` before calling [`start`].
	pub subscribe: Option<origin::Producer>,

	/// The origin (hop) id assigned to the peer, used whenever the peer doesn't
	/// declare one itself. See `Client::with_peer_hop`.
	pub peer_hop: Option<Hop>,

	/// The version of the protocol to use.
	pub version: Version,

	/// The capabilities (and optional request path) we advertise in our SETUP message.
	/// Only sent on versions with a Setup Stream (lite-05+); ignored otherwise.
	/// Its `origin` is filled in here from the attached origin handles.
	pub our_setup: Setup,

	/// The peer's SETUP, when it was already read before [`start`] (e.g. a server that
	/// gated on the client's path via [`accept_setup`]). Seeds the peer-setup slot so
	/// the Setup Stream isn't expected again. `None` reads it from the wire as usual.
	pub peer_setup: Option<AcceptedSetup>,

	/// The session's auth handle, created before [`start`] so a server can take the
	/// peer's token requests during its handshake. Supports AUTH exactly when `version`
	/// does.
	pub auth: crate::auth::Link,
	/// What the peer may make this session hold.
	pub limits: crate::session::Limits,
}

/// Start a lite session.
///
/// Returns the receive-bandwidth consumer (if any) plus the driver that runs the session.
pub fn start(config: Config) -> Result<SessionStart, Error> {
	let Config {
		runtime,
		stats,
		client,
		session,
		setup_stream,
		publish,
		subscribe,
		peer_hop,
		version,
		mut our_setup,
		peer_setup,
		auth,
		limits,
	} = config;

	let recv_bw = bandwidth::Producer::new();

	let recv_bw_consumer = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw.weak()),
	};

	let recv_bw_for_sub = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw),
	};

	// Declare our Hop ID in SETUP so the peer can serve our subscriptions from a
	// route that does not flow through us. Taken from the caller's real handles
	// before the empty-half defaulting below, since those placeholders carry
	// throwaway ids that never appear in a hop chain. The publish identity is what
	// we stamp onto forwarded announcements, so it wins when both halves are wired
	// (they share it in practice).
	if our_setup.hop.is_none() {
		our_setup.hop = publish
			.as_ref()
			.map(|origin| origin.hop())
			.or_else(|| subscribe.as_ref().map(|origin| origin.hop()))
			.filter(|hop| hop.id() != 0);
	}

	// What the peer's connection credential earns by default: publishing what our
	// subscribe half accepts, and subscribing to what our publish half serves. A
	// missing half grants nothing.
	let peer_grant = crate::auth::Grant {
		publish: subscribe.as_ref().map(|origin| origin.allowed()).unwrap_or_default(),
		subscribe: publish.as_ref().map(|origin| origin.allowed()).unwrap_or_default(),
		expires: None,
	};

	// Always run both loops so inbound control (Subscribe/Announce/Probe/Goaway)
	// and GROUP streams are accepted regardless of which halves the caller wired.
	// An unset half gets an empty origin: an empty publish origin announces nothing
	// (and answers the peer's announce-interest with an empty set), and an empty
	// subscribe origin issues no ANNOUNCE_PLEASE.
	let publish = publish.unwrap_or_else(|| {
		origin::Producer::empty(Hop::random())
			.consume()
			.with_stats(stats.clone())
	});
	let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Hop::random()).with_stats(stats.clone()));
	let subscriptions = crate::session::Slots::default().with_stats(publish.stats(), crate::stats::Cap::Subscriptions);
	let announces =
		crate::session::Slots::new(limits.announces).with_stats(subscribe.stats(), crate::stats::Cap::Announces);

	// Publisher and Subscriber each derive their identity from their own
	// attached origin (publish.info / subscribe.info). This is what gets
	// stamped onto outbound hops and checked against incoming hops, so it
	// must be stable across every session that shares the local origin.
	// Required for cross-session cluster loop detection.
	// Shared slot for the peer's SETUP (lite-05+). The subscriber writes it when it
	// reads the peer's Setup stream; capability-gated streams (PROBE) wait on it.
	// When the caller already read it (a gated server accept), seed the slot so the
	// Setup stream isn't expected on the wire again.
	let peer_implementation = crate::setup::PeerImplementation::default();
	let mut peer_setup_slot = PeerSetup::new(client, session.address())
		.offer_ietf(our_setup.ietf)
		.with_implementation(peer_implementation.clone());
	// Without AUTH, the server checks a client's credential before its SETUP, which the
	// client reads only after going live.
	if client && version.has_setup_stream() && !version.has_auth() {
		peer_setup_slot = peer_setup_slot.accepting(auth.clone());
	}
	// A version without a Setup Stream advertises nothing, so there is nothing to wait for.
	if !version.has_setup_stream() {
		peer_implementation.set(None);
	}
	let early = if let Some(accepted) = peer_setup {
		peer_setup_slot.set(accepted.setup);
		accepted.early
	} else {
		Vec::new()
	};
	let peer_setup = peer_setup_slot;

	// GOAWAY wiring: the public Session holds one half (send trigger, received
	// signal), the protocol tasks below hold the other. Only the server may
	// name a redirect URI.
	let (goaway_handle, goaway) = crate::goaway::Handle::new(!client);

	// Read out before the setup machine takes ownership below.
	let our_cost = our_setup.cost;

	// Present the connection's own credential (the empty token) right away, so
	// both sides learn their grant without waiting on the app.
	let setup_token = match version.has_auth() {
		true => Some(auth.present(bytes::Bytes::new(), true)?),
		false => None,
	};

	let estimate = kio::Producer::default();
	let publisher = Publisher::new(PublisherConfig {
		estimate: estimate.weak(),
		runtime: runtime.clone(),
		session: session.clone(),
		origin: publish,
		version,
		peer_setup: peer_setup.clone(),
		goaway: goaway.clone(),
		peer_hop,
		auth: auth.clone(),
		peer_grant,
		client,
		subscriptions,
	});
	let mut subscriber = Subscriber::new(SubscriberConfig {
		runtime: runtime.clone(),
		session: session.clone(),
		origin: subscribe,
		recv_bandwidth: recv_bw_for_sub,
		version,
		peer_setup,
		peer_hop,
		// Local policy for what pulling from this peer costs. Set only when we
		// configured a price; otherwise the subscriber charges what the peer declared
		// for its own egress.
		cost: our_cost,
		going_away: goaway.going_away.clone(),
		auth: auth.clone(),
	});
	subscriber.announces = announces;

	// SendSetup only logs a failure, so refuse a SETUP that cannot encode (one past the
	// size bound, such as a long implementation or path) here rather than leave the peer waiting.
	if version.has_setup_stream() {
		crate::coding::Encode::encode(
			&our_setup,
			&mut crate::coding::Encoder::new(&mut Vec::new(), version.into()),
			version,
		)?;
	}

	let driver = Driver {
		auth: Present {
			stats: stats.clone(),
			runtime: runtime.clone(),
			session: session.clone(),
			version,
			handle: auth.clone(),
			tokens: kio::Tasks::new(),
			violation: Default::default(),
			started: false,
			_setup: setup_token.clone(),
		},
		setup: version
			.has_setup_stream()
			.then(|| SendSetup::new(session.clone(), our_setup, version, stats.clone())),
		goaway: Some(SendGoaway::new(session.clone(), goaway, version, stats.clone())),
		session_stream: setup_stream.map(|stream| stream.with_stats(&stats)),
		ending: None,
		publisher,
		subscriber: SubscriberDriver::new(subscriber, early, client),
		session,
	};

	Ok(SessionStart {
		recv_bandwidth: recv_bw_consumer,
		estimate,
		driver,
		goaway: goaway_handle,
		auth,
		credential: setup_token,
		peer_implementation,
	})
}

/// The lite session driver: one poll function racing every protocol arm, in
/// place of a task set of boxed futures.
pub(crate) struct Driver {
	/// Presenting our tokens, one AUTH stream each.
	auth: Present,
	/// Advertising our capabilities, or `None` once sent (or on a version with no
	/// Setup Stream).
	setup: Option<SendSetup>,
	/// Sending our single GOAWAY if the drain trigger fires, or `None` once done.
	goaway: Option<SendGoaway>,
	/// The legacy session stream (pre-lite-03). Only its *error* ends the race, so
	/// the publisher and subscriber keep running while it sits idle.
	session_stream: Option<Stream<Transport, Version>>,
	/// The first terminal outcome, retained while received streams drain.
	ending: Option<Result<(), Error>>,
	publisher: Publisher,
	subscriber: SubscriberDriver,
	/// For the terminal close: the machine's last act reports the outcome to
	/// the peer through the transport.
	session: Transport,
}

impl Driver {
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.ending.is_none() {
			let res = std::task::ready!(self.poll_protocol(waiter));
			self.auth.handle.close(match &res {
				Ok(()) => Error::Cancel,
				Err(err) => err.clone(),
			});
			match &res {
				Err(err @ (Error::Transport(_) | Error::IdleTimeout)) => {
					tracing::info!("session terminated");
					self.session.abort(err, SessionError::Internal.to_code(), "");
				}
				Err(err) => {
					tracing::warn!(%err, "session error");
					self.session.abort(
						err,
						crate::lite::error::to_session_code(&SessionError::from(err)),
						err.to_string().as_ref(),
					);
				}
				_ => {
					tracing::info!("session closed");
					self.session.abort(&Error::Cancel, SessionError::Cancel.to_code(), "");
				}
			}
			self.ending = Some(res);
		}
		// Closing the transport first bounds every read, including when a control
		// stream ended the protocol while the connection itself was still live.
		// Keep control machines and their subscription entries until data drains,
		// whichever side closed it.
		std::task::ready!(self.subscriber.poll_drain(waiter));
		let res = self.ending.take().expect("draining a terminal session");
		// A clean close, by dropping or draining here or by the peer, cuts off what is
		// still arriving with the cancelled session either way.
		self.subscriber.abort(match &res {
			Err(err) if !self.session.closing().is_local() => err,
			_ => &Error::Session(SessionError::Cancel),
		});
		Poll::Ready(res)
	}

	/// Start withdrawing this session's announcements.
	pub(crate) fn close(&self) {
		self.publisher.close();
	}

	/// Whether no stream still owes the peer data, for a draining close.
	pub(crate) fn drained(&self) -> bool {
		self.publisher.drained()
	}

	fn poll_protocol(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Presenting tokens ends the session only when the peer breaks the protocol.
		if let Poll::Ready(err) = self.auth.poll(waiter) {
			return Poll::Ready(Err(err));
		}

		// The send-side machines never end the session; completion just retires them.
		if let Some(setup) = &mut self.setup
			&& setup.poll(waiter).is_ready()
		{
			self.setup = None;
		}
		if let Some(goaway) = &mut self.goaway
			&& goaway.poll(waiter).is_ready()
		{
			self.goaway = None;
		}

		if let Some(stream) = &mut self.session_stream
			&& let Poll::Ready(err) = poll_session_stream(stream, waiter)
		{
			return Poll::Ready(Err(err));
		}
		if let Poll::Ready(res) = self.publisher.poll(waiter) {
			return Poll::Ready(res);
		}
		if let Poll::Ready(res) = self.subscriber.poll(waiter) {
			return Poll::Ready(res);
		}
		Poll::Pending
	}
}

impl Drop for Driver {
	fn drop(&mut self) {
		// Dropped without finishing: release anything still waiting on a token.
		self.auth.handle.close(Error::Cancel);
	}
}

/// Opens one AUTH stream per token this side presents.
struct Present {
	stats: crate::stats::Session,
	runtime: crate::time::Deadlines,
	session: Transport,
	version: Version,
	handle: crate::auth::Link,
	tokens: kio::Tasks<PresentToken>,
	/// Set by a token whose peer broke the protocol, which ends the session.
	violation: kio::Shared<Option<Error>>,
	/// Whether the first poll decided who answers the peer's tokens.
	started: bool,
	/// The connection's own credential, held for the life of the session.
	_setup: Option<crate::auth::Credential>,
}

impl Present {
	/// Resolve only when a token's peer broke the protocol.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		if !self.started {
			// Decided once, before any AUTH stream can be accepted: the app took the
			// requests before running the driver, or the session answers itself.
			let _ = self.handle.acceptor();
			self.started = true;
		}
		while let Poll::Ready(Some((id, token))) = self.handle.poll_opening(waiter) {
			self.tokens.push(PresentToken {
				stats: self.stats.clone(),
				runtime: self.runtime.clone(),
				session: self.session.clone(),
				version: self.version,
				handle: self.handle.clone(),
				violation: self.violation.clone(),
				id,
				state: PresentState::Open { token, answered: false },
			});
		}
		let _ = self.tokens.poll(waiter);
		// A token sets it from inside the poll above, so nothing needs waking.
		match self.violation.lock().take() {
			Some(err) => Poll::Ready(err),
			None => Poll::Pending,
		}
	}
}

/// One token's AUTH stream: send the token, then track the grant until either
/// side ends it.
struct PresentToken {
	stats: crate::stats::Session,
	runtime: crate::time::Deadlines,
	session: Transport,
	version: Version,
	handle: crate::auth::Link,
	violation: kio::Shared<Option<Error>>,
	id: u64,
	state: PresentState,
}

enum PresentState {
	Open {
		token: bytes::Bytes,
		/// The peer already replied to this token, so it knows AUTH: a retry's stream lost
		/// before its reply is not the peer predating AUTH.
		answered: bool,
	},
	Run {
		stream: Stream<Transport, Version>,
		answered: bool,
	},
	/// Refused for a reason that may pass, or its stream lost: present it again once the
	/// deadline fires.
	Retry {
		deadline: crate::time::Deadline,
		answered: bool,
	},
	Done,
}

impl kio::Task for PresentToken {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let err = ready!(self.poll_present(waiter));
			let answered = matches!(
				self.state,
				PresentState::Open { answered: true, .. }
					| PresentState::Run { answered: true, .. }
					| PresentState::Retry { answered: true, .. }
			);
			if let PresentState::Run { stream, .. } = std::mem::replace(&mut self.state, PresentState::Done) {
				stream.writer.abort(&Error::Cancel);
			}
			let err = match err {
				// A peer that predates AUTH resets a stream type it does not know, and one
				// that takes no tokens in band refuses the same way: either way, before any
				// reply, this token earns nothing here.
				Error::Stream(_) if !answered => Error::Unsupported,
				// A malformed reply, truncated ones included, is the peer breaking the session
				// protocol, not refusing a token.
				err => crate::auth::protocol_error(err),
			};
			if let Some(delay) = self.handle.retry(self.id, &err) {
				tracing::debug!(%err, ?delay, "auth token presenting again");
				self.state = PresentState::Retry {
					deadline: crate::time::Deadline::after(&self.runtime, delay),
					answered,
				};
				continue;
			}
			match &err {
				Error::Cancel | Error::Unsupported | Error::Transport(_) | Error::IdleTimeout | Error::Session(_) => {
					tracing::debug!(%err, "auth token ended")
				}
				err => tracing::warn!(%err, "auth token ended"),
			}
			if let Error::ProtocolViolation = err {
				self.violation.lock().get_or_insert(err.clone());
			}
			self.handle.ended(self.id, err);
			return Poll::Ready(());
		}
	}
}

impl PresentToken {
	/// Run the stream, resolving with why the token ended.
	fn poll_present(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		let mut budget = kio::coop::Budget::new(32);
		loop {
			match &mut self.state {
				PresentState::Open { token, answered } => {
					// After a GOAWAY, sent or received, the peer must not see new streams.
					if !self.handle.active() {
						return Poll::Ready(Error::GoingAway);
					}
					let token = token.clone();
					let answered = *answered;
					let mut stream = match ready!(Stream::poll_open_request(&self.session, self.version, waiter)) {
						Ok(stream) => stream,
						Err(err) => return Poll::Ready(err),
					};
					stream = stream.with_stats(&self.stats);
					let res = stream
						.writer
						.buffer(&super::ControlType::Auth)
						.and_then(|()| stream.writer.buffer(&super::Auth { token }));
					if let Err(err) = res {
						return Poll::Ready(err);
					}
					self.state = PresentState::Run { stream, answered };
				}
				PresentState::Run { stream, answered } => {
					if let Err(err) = ready!(stream.writer.poll_flush(waiter)) {
						return Poll::Ready(err);
					}
					// Withdrawn: closing the stream is what tells the peer.
					if self.handle.poll_withdrawn(self.id, waiter).is_ready() {
						return Poll::Ready(Error::Cancel);
					}
					let reply = match ready!(stream.reader.poll_decode_maybe::<super::AuthReply>(waiter)) {
						Ok(Some(reply)) => reply,
						// The peer ended the grant without revoking it, or closed without
						// ever answering (mapped to unsupported by the caller).
						Ok(None) if *answered => return Poll::Ready(Error::Cancel),
						Ok(None) => return Poll::Ready(Error::Unsupported),
						Err(err) => return Poll::Ready(err),
					};
					match reply {
						super::AuthReply::Ok(ok) => {
							let now = self.runtime.now();
							// The expiry is the peer's number: one past the local clock's range
							// is malformed, not a reason to panic.
							let expires = match ok.expires.map(|expires| now.checked_add(expires)) {
								Some(None) => return Poll::Ready(Error::ProtocolViolation),
								expires => expires.flatten(),
							};
							*answered = true;
							self.handle.granted(
								self.id,
								crate::auth::Grant {
									publish: ok.publish,
									subscribe: ok.subscribe,
									expires,
								},
							);
							// Each grant recomputes the session's permits, so a peer streaming
							// them must not hold the session's other tasks.
							ready!(budget.poll_yield(waiter));
						}
						super::AuthReply::Error(refused) => {
							// Session codes are u32 everywhere else, so a wider one is malformed.
							let Ok(code) = u32::try_from(refused.code) else {
								return Poll::Ready(Error::ProtocolViolation);
							};
							let err = match crate::lite::error::from_session_code(code) {
								crate::SessionError::Unsupported => Error::Unsupported,
								code => Error::Session(code),
							};
							tracing::warn!(%err, reason = %refused.reason, "auth token refused");
							if !matches!(err, Error::Unsupported) {
								self.handle.refused(self.id);
							}
							*answered = true;
							return Poll::Ready(err);
						}
					}
				}
				PresentState::Retry { deadline, answered } => {
					if self.handle.poll_withdrawn(self.id, waiter).is_ready() {
						return Poll::Ready(Error::Cancel);
					}
					ready!(deadline.poll(waiter));
					let answered = *answered;
					let token = match self.handle.reopen(self.id) {
						Ok(token) => token,
						Err(err) => return Poll::Ready(err),
					};
					self.state = PresentState::Open { token, answered };
				}
				PresentState::Done => return Poll::Ready(Error::Cancel),
			}
		}
	}
}

/// Drain SessionInfo updates off the legacy session stream, resolving only when
/// the stream dies (a FIN counts: the peer abandoned the session).
// TODO do something useful with the updates
fn poll_session_stream(stream: &mut Stream<Transport, Version>, waiter: &kio::Waiter) -> Poll<Error> {
	// A yield only skips this arm: the rest of the driver still runs this turn.
	let mut budget = kio::coop::Budget::new(32);
	loop {
		ready!(budget.poll_yield(waiter));
		match ready!(stream.reader.poll_decode_maybe::<SessionInfo>(waiter)) {
			Ok(Some(_info)) => {}
			Ok(None) => return Poll::Ready(Error::Cancel),
			Err(err) => return Poll::Ready(err),
		}
	}
}

/// Advertise our capabilities on a uni Setup Stream, then FIN. Best-effort: an
/// error is logged and the machine finishes; the peer falls back to "no
/// capabilities" for us.
struct SendSetup {
	stats: crate::stats::Session,
	version: Version,
	session: Transport,
	setup: Box<Setup>,
	state: SendSetupState,
}

#[kio::machine(self.state: SendSetupState)]
impl SendSetup {
	fn new(session: Transport, setup: Setup, version: Version, stats: crate::stats::Session) -> Self {
		Self {
			stats,
			version,
			session,
			setup: Box::new(setup),
			state: SendSetupState::Start,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		match ready!(self.poll_send(waiter)) {
			Ok(()) => {}
			Err(err) => tracing::debug!(%err, "failed to send setup"),
		}
		Poll::Ready(())
	}

	fn poll_send(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Waiting for stream credit on our own session handle.
		let stream = wait!(Open => self.session.poll_open_uni(waiter, Default::default()))
			.map_err(crate::lite::error::from_transport)?;
		let mut writer: Writer<crate::transport::boxed::SendStream, Version> =
			Writer::new(stream, self.version).with_stats(&self.stats);
		writer.buffer(&super::DataType::Setup)?;
		writer.buffer(&*self.setup)?;
		// Flushing the buffered SETUP, then FIN and wait for the acknowledgement (a
		// reset racing the FIN would discard the unacked message).
		wait!(Flush => writer.poll_flush(waiter))?;
		writer.finish()?;
		Poll::Ready(wait!(Close => writer.poll_closed(waiter)))
	}
}

/// Send our single GOAWAY when the drain trigger fires. The session supervisor
/// enforces the deadline independently, including on versions with no GOAWAY message.
struct SendGoaway {
	stats: crate::stats::Session,
	version: Version,
	goaway: crate::goaway::Protocol,
	/// Opening a stream and watching the session close each need their own handle.
	closed: Transport,
	session: Transport,
	state: SendGoawayState,
}

#[kio::machine(self.state: SendGoawayState)]
impl SendGoaway {
	fn new(
		session: Transport,
		goaway: crate::goaway::Protocol,
		version: Version,
		stats: crate::stats::Session,
	) -> Self {
		Self {
			stats,
			version,
			goaway,
			closed: session.clone(),
			session,
			state: SendGoawayState::Start,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		#[every_poll]
		if self.closed.poll_closed(waiter).is_ready() {
			return Poll::Ready(());
		}

		let payload: crate::goaway::Goaway = match wait!(Trigger => self.goaway.poll_triggered(waiter)) {
			Some(payload) => payload,
			None => return Poll::Ready(()),
		};
		if !self.version.has_goaway() {
			return Poll::Ready(());
		}
		let mut stream: Stream<Transport, Version> = match wait!(Open => Stream::poll_open(&self.session, self.version, waiter))
		{
			Ok(stream) => stream,
			Err(err) => {
				tracing::warn!(%err, "failed to send goaway");
				return Poll::Ready(());
			}
		};
		stream = stream.with_stats(&self.stats);
		let msg = super::Goaway {
			uri: std::borrow::Cow::Borrowed(payload.uri.as_str()),
		};
		if let Err(err) = stream
			.writer
			.buffer(&super::ControlType::Goaway)
			.and_then(|()| stream.writer.buffer(&msg))
		{
			tracing::warn!(%err, "failed to send goaway");
			return Poll::Ready(());
		}

		// Keep the writer through FIN acknowledgement, since dropping it resets the stream.
		let finished: Result<(), Error> =
			wait!(Flush => stream.writer.poll_flush(waiter)).and_then(|()| stream.writer.finish());
		let closed = wait!(Close => match finished {
			Ok(()) => stream.writer.poll_closed(waiter),
			Err(_) => Poll::Ready(Ok(())),
		});
		if let Err(err) = finished.and(closed) {
			tracing::warn!(%err, "failed to send goaway");
		}
		Poll::Ready(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;
	use crate::lite::test_transport::ScriptedSession;

	#[moq_clock::test]
	async fn malformed_auth_replies_close_the_session() {
		// Unknown reply, invalid code, truncated error, and trailing body bytes.
		for script in [
			vec![99],
			vec![1, 9, 0xc0, 0, 0, 1, 0, 0, 0, 0, 0],
			vec![1, 1, 1],
			vec![1, 3, 1, 0, 0],
		] {
			malformed_auth_reply(script).await;
		}
	}

	/// A protocol error on a live connection must end partial receive reads before
	/// waiting for them, otherwise receive draining could wait for new peer data.
	#[moq_clock::test]
	async fn malformed_auth_closes_before_draining_a_partial_group_header() {
		assert_session_closes(
			ScriptedSession::per_stream(vec![vec![99]]).with_incoming_unis(vec![vec![0]]),
			Setup::default(),
		)
		.await;
	}

	/// After close, no stream can deliver the peer SETUP a GROUP stream waits on to read
	/// its header, whether the SETUP stream was truncated or never opened.
	#[moq_clock::test]
	async fn drain_ends_when_groups_wait_on_an_undeliverable_setup() {
		let version = Version::Lite07;
		let mut group = Vec::new();
		DataType::Group
			.encode(&mut crate::coding::Encoder::new(&mut group, version.into()), version)
			.unwrap();
		let mut setup = Vec::new();
		DataType::Setup
			.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
			.unwrap();
		let ietf = Setup {
			ietf: true,
			..Default::default()
		};
		for unis in [vec![group.clone(), setup], vec![group]] {
			assert_session_closes(
				ScriptedSession::per_stream(vec![vec![99]]).with_incoming_unis(unis),
				ietf.clone(),
			)
			.await;
		}
	}

	async fn malformed_auth_reply(script: Vec<u8>) {
		// The setup token's AUTH stream is the first one the session opens.
		let transport = ScriptedSession::per_stream(vec![script]);
		let log = transport.log.clone();

		let start = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			client: true,
			limits: Default::default(),
			session: crate::driver::Transport::new(transport.clone()),
			setup_stream: None,
			publish: None,
			subscribe: None,
			peer_hop: None,
			version: Version::Lite07,
			our_setup: Setup::default(),
			peer_setup: None,
			auth: crate::auth::Link::new(true),
		})
		.unwrap();
		let mut driver = start.driver;

		let res = moq_clock::timeout(
			std::time::Duration::from_secs(10),
			kio::wait(|waiter| driver.poll(waiter)),
		)
		.await
		.expect("the session outlived a malformed AUTH_ERROR");
		assert!(matches!(res, Err(Error::ProtocolViolation)), "{res:?}");
		assert_eq!(log.closes()[0].0, SessionError::ProtocolViolation.to_code());
	}

	/// An AUTH_OK granting nothing, forever.
	fn empty_grant(version: Version) -> Vec<u8> {
		let mut grant = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut grant, version.into());
		w.varint(0).unwrap();
		let prefix = w.prefix_varint();
		for _ in 0..3 {
			w.varint(0).unwrap();
		}
		w.fill(prefix).unwrap();
		grant
	}

	/// Run a client session on a shared handle over `transport` for five simulated seconds,
	/// presenting one token, and return it after checking the session still runs.
	async fn present_on_shared_handle(transport: ScriptedSession) -> crate::auth::Credential {
		let handle = crate::auth::Handle::new();
		let start = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			client: true,
			limits: Default::default(),
			session: crate::driver::Transport::new(transport),
			setup_stream: None,
			publish: None,
			subscribe: None,
			peer_hop: None,
			version: Version::Lite07,
			our_setup: Setup::default(),
			peer_setup: None,
			auth: crate::auth::Link::attach(&handle, true, false).unwrap(),
		})
		.unwrap();
		let mut driver = start.driver;
		let token = handle.present(bytes::Bytes::from_static(b"token")).unwrap();

		let res = moq_clock::timeout(
			std::time::Duration::from_secs(5),
			kio::wait(|waiter| driver.poll(waiter)),
		)
		.await;
		assert!(res.is_err(), "the session ended: {res:?}");
		token
	}

	/// A granted token whose stream the peer resets is presented again on the same session,
	/// backing off while each attempt is lost before a reply: at 0s, 1s, and 3s.
	#[moq_clock::test]
	async fn a_lost_stream_is_presented_again() {
		let grant = empty_grant(Version::Lite07);
		let transport = ScriptedSession::per_stream(vec![grant.clone(), grant]);
		transport.close(crate::lite::test_transport::Close::ResetStream(
			crate::StreamError::Internal.to_code(),
		));
		let log = transport.log.clone();
		let token = present_on_shared_handle(transport).await;

		let writes = log.writes.lock().unwrap().clone();
		assert_eq!(writes.windows(5).filter(|window| *window == b"token").count(), 3);
		assert!(
			futures::FutureExt::now_or_never(token.closed()).is_none(),
			"a lost stream ended the token"
		);
	}

	/// A retry's stream that ends before its reply is lost, not a peer without AUTH: the
	/// peer already answered the token, so it is presented again, backing off from the
	/// refusal's delay: at 0s, 1s, and 3s.
	#[moq_clock::test]
	async fn a_lost_retry_is_presented_again() {
		let version = Version::Lite07;
		let mut busy = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut busy, version.into());
		w.varint(1).unwrap();
		let prefix = w.prefix_varint();
		w.varint(SessionError::TooManyRequests.to_code().into()).unwrap();
		w.bytes(b"busy").unwrap();
		w.fill(prefix).unwrap();

		// The setup token is granted and then closed, which settles it. Ours is refused
		// transiently, and every retry's stream ends without a reply.
		let transport = ScriptedSession::per_stream_eof(vec![empty_grant(version), busy]);
		let log = transport.log.clone();
		let token = present_on_shared_handle(transport).await;

		assert_eq!(log.bi_opens(), 4, "the setup token once, ours three times");
		assert!(
			futures::FutureExt::now_or_never(token.closed()).is_none(),
			"a lost retry ended the token"
		);
	}

	#[moq_clock::test]
	async fn malformed_presenter_auth_closes_the_session() {
		for script in [vec![7, 1, 2], vec![7, 1, 0, 0]] {
			let transport = ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![script]);
			let log = transport.log.clone();
			let start = start(Config {
				runtime: crate::time::sim(),
				stats: Default::default(),
				client: false,
				limits: Default::default(),
				session: crate::driver::Transport::new(transport.clone()),
				setup_stream: None,
				publish: None,
				subscribe: None,
				peer_hop: None,
				version: Version::Lite07,
				our_setup: Setup::default(),
				peer_setup: None,
				auth: crate::auth::Link::new(true),
			})
			.unwrap();
			let mut driver = start.driver;
			let _ = driver.poll(&kio::Waiter::noop());
			assert_eq!(
				log.closes().first().map(|close| close.0),
				Some(SessionError::ProtocolViolation.to_code())
			);
		}
	}

	#[moq_clock::test]
	async fn malformed_auth_ok_closes_the_session() {
		for pattern in [
			b"*/**".as_slice(),
			b"/room",
			vec!["room"; 33].join("/").as_bytes(),
			b"\xff",
		] {
			let mut script = Vec::new();
			let mut w = crate::coding::Encoder::new(&mut script, Version::Lite07.into());
			w.varint(0).unwrap();
			let prefix = w.prefix_varint();
			w.varint(1).unwrap();
			w.bytes(pattern).unwrap();
			w.varint(0).unwrap();
			w.varint(0).unwrap();
			w.fill(prefix).unwrap();
			assert_malformed_reply_closes(script).await;
		}
		let mut script = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut script, Version::Lite07.into());
		w.varint(0).unwrap();
		let prefix = w.prefix_varint();
		w.varint(0).unwrap();
		w.varint(0).unwrap();
		w.varint(1 << 53).unwrap();
		w.fill(prefix).unwrap();
		assert_malformed_reply_closes(script).await;
	}

	/// A reply cut off inside its declared size fails to decode, unlike a peer that
	/// resets before answering.
	#[moq_clock::test]
	async fn truncated_auth_reply_closes_the_session() {
		let mut script = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut script, Version::Lite07.into());
		w.varint(0).unwrap();
		w.varint(4).unwrap();
		w.varint(0).unwrap();
		assert_session_closes(ScriptedSession::per_stream_eof(vec![script]), Setup::default()).await;
	}

	#[moq_clock::test]
	async fn malformed_auth_error_reasons_close_the_session() {
		for reason in [vec![0xff], vec![b'a'; 8193]] {
			let mut script = Vec::new();
			let mut w = crate::coding::Encoder::new(&mut script, Version::Lite07.into());
			w.varint(1).unwrap();
			let prefix = w.prefix_varint();
			w.varint(SessionError::Unauthorized.to_code().into()).unwrap();
			w.bytes(&reason).unwrap();
			w.fill(prefix).unwrap();
			assert_malformed_reply_closes(script).await;
		}
	}

	async fn assert_malformed_reply_closes(script: Vec<u8>) {
		assert_session_closes(ScriptedSession::per_stream(vec![script]), Setup::default()).await;
	}

	async fn assert_session_closes(transport: ScriptedSession, our_setup: Setup) {
		let log = transport.log.clone();
		let start = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			client: true,
			limits: Default::default(),
			session: crate::driver::Transport::new(transport.clone()),
			setup_stream: None,
			publish: None,
			subscribe: None,
			peer_hop: None,
			version: Version::Lite07,
			our_setup,
			peer_setup: None,
			auth: crate::auth::Link::new(true),
		})
		.unwrap();
		let mut driver = start.driver;
		let res = moq_clock::timeout(
			std::time::Duration::from_secs(10),
			kio::wait(|waiter| driver.poll(waiter)),
		)
		.await
		.expect("the session outlived a malformed AUTH reply");
		assert!(matches!(res, Err(Error::ProtocolViolation)), "{res:?}");
		assert_eq!(log.closes()[0].0, SessionError::ProtocolViolation.to_code());
	}

	#[moq_clock::test]
	async fn accept_setup_does_not_wait_on_an_early_stream_header() {
		let version = Version::Lite05;
		let mut setup = Vec::new();
		DataType::Setup
			.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
			.unwrap();
		Setup::default()
			.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
			.unwrap();
		let session =
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![Vec::new(), setup]);
		let accepted = accept_setup(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		assert_eq!(accepted.early.len(), 1);
	}

	#[moq_clock::test]
	async fn accept_setup_refuses_two_incomplete_setup_streams() {
		use futures::FutureExt;
		let session =
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![vec![1], vec![1]]);
		assert!(matches!(
			accept_setup(&mut crate::driver::Transport::new(session.clone()), Version::Lite05).now_or_never(),
			Some(Err(Error::ProtocolViolation))
		));
	}

	#[moq_clock::test]
	async fn duplicate_setup_closes_the_session() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for complete in [false, true] {
				let mut first = Vec::new();
				DataType::Setup
					.encode(&mut crate::coding::Encoder::new(&mut first, version.into()), version)
					.unwrap();
				if complete {
					Setup::default()
						.encode(&mut crate::coding::Encoder::new(&mut first, version.into()), version)
						.unwrap();
				}
				let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
					.with_incoming_unis(vec![first.clone(), first]);
				let log = session.log.clone();
				let mut started = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					setup_stream: None,
					publish: None,
					subscribe: None,
					peer_hop: None,
					version,
					our_setup: Setup::default(),
					peer_setup: None,
					client: false,
					auth: crate::auth::Link::new(version.has_auth()),
					limits: Default::default(),
				})
				.unwrap();
				let _ = started.driver.poll(&kio::Waiter::noop());
				assert!(
					log.closes()
						.iter()
						.any(|(code, _)| *code == SessionError::ProtocolViolation.to_code()),
					"{version:?}, complete={complete}: duplicate SETUP must close the session"
				);
			}
		}
	}

	/// A SETUP that fails to decode leaves nothing to wait on, since the Setup Stream is
	/// already claimed.
	#[moq_clock::test]
	async fn malformed_setup_closes_the_session() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut truncated = Vec::new();
			DataType::Setup
				.encode(
					&mut crate::coding::Encoder::new(&mut truncated, version.into()),
					version,
				)
				.unwrap();

			// Two empty parameters with the same ID.
			let mut repeated = truncated.clone();
			let mut w = crate::coding::Encoder::new(&mut repeated, version.into());
			let prefix = w.prefix_varint();
			w.varint(2).unwrap();
			for _ in 0..2 {
				w.varint(0).unwrap();
				w.bytes(&[]).unwrap();
			}
			w.fill(prefix).unwrap();

			for (case, setup) in [("truncated", truncated), ("repeated parameter", repeated)] {
				let session =
					crate::lite::test_transport::ScriptedSession::eof(Vec::new()).with_incoming_unis(vec![setup]);
				let log = session.log.clone();
				let mut started = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					setup_stream: None,
					publish: None,
					subscribe: None,
					peer_hop: None,
					version,
					our_setup: Setup::default(),
					peer_setup: None,
					client: false,
					auth: crate::auth::Link::new(version.has_auth()),
					limits: Default::default(),
				})
				.unwrap();
				let _ = started.driver.poll(&kio::Waiter::noop());
				assert!(
					log.closes()
						.iter()
						.any(|(code, _)| *code == SessionError::ProtocolViolation.to_code()),
					"{version:?}, {case}: SETUP must close the session"
				);
			}
		}
	}

	fn raw_session(
		transport: ScriptedSession,
		version: Version,
		client: bool,
		peer_setup: Option<AcceptedSetup>,
	) -> SessionStart {
		start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			client,
			session: crate::driver::Transport::new(transport.clone()),
			setup_stream: None,
			publish: None,
			subscribe: None,
			peer_hop: None,
			version,
			our_setup: Setup::default(),
			peer_setup,
			auth: crate::auth::Link::new(version.has_auth()),
			limits: Default::default(),
		})
		.unwrap()
	}

	#[moq_clock::test]
	async fn server_setup_client_parameters_close_the_session() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for body in [vec![1, 2, 0], vec![1, 3, 1, 0], vec![1, 3, 1, 1], vec![1, 3, 1, 3]] {
				let mut script = vec![1, body.len() as u8];
				script.extend(body);
				let transport = ScriptedSession::new(Vec::new()).with_incoming_unis(vec![script]);
				let log = transport.log.clone();
				let mut started = raw_session(transport, version, true, None);
				let _ = started.driver.poll(&kio::Waiter::noop());
				assert!(
					log.closes()
						.iter()
						.any(|(code, _)| *code == SessionError::ProtocolViolation.to_code()),
					"{version}"
				);
			}
		}
	}

	#[moq_clock::test]
	async fn unknown_client_role_remains_bidirectional() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let transport = ScriptedSession::new(Vec::new()).with_incoming_unis(vec![vec![1, 4, 1, 3, 1, 3]]);
			let setup = accept_setup(&mut crate::driver::Transport::new(transport.clone()), version)
				.await
				.unwrap();
			assert_eq!(setup.setup.role, None);
			let log = transport.log.clone();
			let mut started = raw_session(transport, version, false, Some(setup));
			let _ = started.driver.poll(&kio::Waiter::noop());
			assert!(log.closes().is_empty(), "{version}");
		}
	}

	#[moq_clock::test]
	async fn malformed_probe_reports_close_the_session() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let transport = if version.has_auth() {
				let grant = super::super::AuthReply::Ok(super::super::AuthOk {
					publish: Default::default(),
					subscribe: Default::default(),
					expires: None,
				});
				ScriptedSession::per_stream(vec![grant.encode_bytes(version).unwrap().to_vec(), vec![0]])
			} else {
				ScriptedSession::new(vec![0])
			};
			let log = transport.log.clone();
			let peer_setup = AcceptedSetup {
				setup: Setup {
					probe: super::super::ProbeLevel::Report,
					..Default::default()
				},
				size: 0,
				early: Vec::new(),
			};
			let mut started = raw_session(transport, version, true, Some(peer_setup));
			let _bandwidth = started.recv_bandwidth.take().unwrap().consume();
			let _ = started.driver.poll(&kio::Waiter::noop());
			assert!(
				log.closes()
					.iter()
					.any(|(code, _)| *code == SessionError::ProtocolViolation.to_code()),
				"{version}"
			);
		}
	}

	#[moq_clock::test]
	async fn malformed_group_headers_remain_per_stream() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let transport = ScriptedSession::new(Vec::new()).with_incoming_unis(vec![vec![0, 0]]);
			let log = transport.log.clone();
			let mut started = raw_session(transport, version, true, None);
			let _ = started.driver.poll(&kio::Waiter::noop());
			assert!(log.closes().is_empty(), "{version}");
			assert!(!log.stops().is_empty(), "{version}: data stream was not refused");
		}
	}

	// GOAWAY is not a request: refusing it without credit would leave the peer
	// unaware that we are draining.
	#[moq_clock::test]
	async fn goaway_waits_for_stream_credit() {
		let transport = ScriptedSession::new(Vec::new()).without_credit();
		let log = transport.log.clone();
		let mut server = raw_session(transport.clone(), Version::Lite07, false, None);
		server.goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		let _ = server.driver.poll(&kio::Waiter::noop());
		assert_eq!(log.bi_opens(), 0, "GOAWAY waits for stream credit");
		transport.grant_credit();
		let _ = server.driver.poll(&kio::Waiter::noop());
		assert_eq!(log.bi_opens(), 1, "GOAWAY stream was not opened");
		assert!(log.closes().is_empty());
	}

	#[moq_clock::test]
	async fn clients_never_emit_a_goaway_redirect() {
		for version in [Version::Lite04, Version::Lite05, Version::Lite06, Version::Lite07] {
			let client = raw_session(ScriptedSession::new(Vec::new()), version, true, None);
			assert!(
				matches!(
					client
						.goaway
						.producer()
						.send(crate::goaway::Goaway::redirect("https://other.example/")),
					Err(Error::ProtocolViolation)
				),
				"{version}"
			);
			let server = raw_session(ScriptedSession::new(Vec::new()), version, false, None);
			assert!(
				server
					.goaway
					.producer()
					.send(crate::goaway::Goaway::redirect("https://other.example/"))
					.is_ok(),
				"{version}"
			);
		}
	}
}
