use crate::origin;
use crate::{
	Error, Hop, SessionError, bandwidth,
	coding::{Reader, Stream, Writer},
	lite::SessionInfo,
};

use std::task::{Context, Poll, ready};

use super::{
	DataType, PeerSetup, Publisher, PublisherConfig, Setup, Subscriber, SubscriberConfig, SubscriberDriver, Version,
};

pub(crate) struct SessionStart<S: crate::transport::poll::Session> {
	pub recv_bandwidth: Option<bandwidth::Consumer>,
	/// The session's protocol machine, named so its `Send`-ness stays inferred
	/// from the transport instead of being fixed by a box.
	pub driver: Driver<S>,
	/// The session-side GOAWAY halves, stored on the public [`crate::Session`].
	pub goaway: crate::goaway::Handle,
	/// The session's AUTH tokens and grants, stored on the public [`crate::Session`].
	pub auth: crate::auth::Handle,
}

/// Server: read the peer's single SETUP message off its Setup Stream before starting
/// the session, so the caller can inspect the advertised path (and gate on it) before
/// serving. lite-05+ only.
///
/// Blocks on the peer's Setup Stream, which every lite-05+ endpoint opens at startup.
/// Almost always the first unidirectional stream; any other uni stream that races
/// ahead of it is `STOP_SENDING`-ed and skipped (we don't support proactive uni
/// PUBLISH, so nothing legitimate precedes the SETUP today). The eventual home for
/// out-of-order tolerance is the full session loop with deferred origin binding.
///
/// Pass the returned [`Setup`] to [`start`] as its `peer_setup` so PROBE gating still
/// resolves without re-reading the (consumed) stream.
pub async fn accept_setup<S: crate::transport::poll::Session>(
	session: &mut S,
	version: Version,
) -> Result<Setup, Error> {
	loop {
		let stream = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader = Reader::new(stream, version);

		match reader.decode::<DataType>().await? {
			DataType::Setup => return reader.decode::<Setup>().await,
			// A non-SETUP uni stream this early is unexpected (GROUP needs a prior
			// subscribe). Reject it and keep waiting rather than failing the session.
			_ => reader.abort(&Error::UnexpectedStream),
		}
	}
}

/// Everything one moq-lite session needs to start.
pub struct Config<S: crate::transport::poll::Session> {
	/// The runtime that arms the session's timers.
	pub runtime: crate::time::Clock,

	/// Whether we dialed the session. Only the dialing side aborts on a publication
	/// its grant does not cover: a server's publish origin is everything the peer
	/// may read, not what it intends to push.
	pub client: bool,

	/// The transport carrying the session. Cloned into every loop that outlives
	/// [`start`], so the connection closes when the last of them drops.
	pub session: S,

	/// The stream used to set up the session, after exchanging setup messages.
	/// NOTE: No longer used in draft-03.
	pub setup_stream: Option<Stream<S, Version>>,

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
	pub peer_setup: Option<Setup>,

	/// The session's auth handle, created before [`start`] so a server can take the
	/// peer's token requests during its handshake. Supports AUTH exactly when `version`
	/// does.
	pub auth: crate::auth::Handle,
}

/// Start a lite session.
///
/// Returns the receive-bandwidth consumer (if any) plus the driver that runs the session.
pub fn start<S>(config: Config<S>) -> Result<SessionStart<S>, Error>
where
	S: crate::transport::poll::Session,
{
	let Config {
		runtime,
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
	} = config;

	let recv_bw = bandwidth::Producer::new();

	let recv_bw_consumer = match version {
		Version::Lite01 | Version::Lite02 => None,
		_ => Some(recv_bw.consume()),
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
	let publish = publish.unwrap_or_else(|| origin::Producer::empty(Hop::random()).consume());
	let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Hop::random()));

	// Publisher and Subscriber each derive their identity from their own
	// attached origin (publish.info / subscribe.info). This is what gets
	// stamped onto outbound hops and checked against incoming hops, so it
	// must be stable across every session that shares the local origin.
	// Required for cross-session cluster loop detection.
	// Shared slot for the peer's SETUP (lite-05+). The subscriber writes it when it
	// reads the peer's Setup stream; capability-gated streams (PROBE) wait on it.
	// When the caller already read it (a gated server accept), seed the slot so the
	// Setup stream isn't expected on the wire again.
	let peer_setup_slot = PeerSetup::default();
	if let Some(setup) = peer_setup {
		peer_setup_slot.set(setup);
	}
	let peer_setup = peer_setup_slot;

	// GOAWAY wiring: the public Session holds one half (send trigger, received
	// signal), the protocol tasks below hold the other. moq-lite lets either side
	// name a redirect URI, unlike moq-transport.
	let (goaway_handle, goaway) = crate::goaway::Handle::new(true);

	// Read out before the setup machine takes ownership below.
	let our_cost = our_setup.cost;

	// Present the connection's own credential (the empty token) right away, so
	// both sides learn their grant without waiting on the app.
	let setup_token = match version.has_auth() {
		true => Some(auth.present(bytes::Bytes::new(), true)?),
		false => None,
	};

	let publisher = Publisher::new(PublisherConfig {
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
	});
	let subscriber = Subscriber::new(SubscriberConfig {
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

	let driver = Driver {
		auth: Present {
			runtime: runtime.clone(),
			session: session.clone(),
			version,
			handle: auth.clone(),
			going_away: goaway.going_away.clone(),
			tokens: kio::Tasks::new(),
			started: false,
			_setup: setup_token,
		},
		setup: version
			.has_setup_stream()
			.then(|| SendSetup::new(session.clone(), our_setup, version)),
		goaway: Some(SendGoaway::new(runtime, session.clone(), goaway, version)),
		session_stream: setup_stream,
		publisher,
		subscriber: SubscriberDriver::new(subscriber),
		session,
	};

	Ok(SessionStart {
		recv_bandwidth: recv_bw_consumer,
		driver,
		goaway: goaway_handle,
		auth,
	})
}

/// The lite session driver: one poll function racing every protocol arm, in
/// place of a task set of boxed futures.
pub(crate) struct Driver<S: crate::transport::poll::Session> {
	/// Presenting our tokens, one AUTH stream each.
	auth: Present<S>,
	/// Advertising our capabilities, or `None` once sent (or on a version with no
	/// Setup Stream).
	setup: Option<SendSetup<S>>,
	/// Sending our single GOAWAY if the drain trigger fires, or `None` once done.
	goaway: Option<SendGoaway<S>>,
	/// The legacy session stream (pre-lite-03). Only its *error* ends the race, so
	/// the publisher and subscriber keep running while it sits idle.
	session_stream: Option<Stream<S, Version>>,
	publisher: Publisher<S>,
	subscriber: SubscriberDriver<S>,
	/// For the terminal close: the machine's last act reports the outcome to
	/// the peer through the transport.
	session: S,
}

impl<S> Driver<S>
where
	S: crate::transport::poll::Session,
{
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let res = std::task::ready!(self.poll_protocol(waiter));
		self.auth.handle.close(match &res {
			Ok(()) => Error::Cancel,
			Err(err) => err.clone(),
		});
		match &res {
			Err(Error::Transport(_)) => {
				tracing::info!("session terminated");
				self.session.close(SessionError::Internal.to_code(), "");
			}
			Err(err) => {
				tracing::warn!(%err, "session error");
				self.session
					.close(SessionError::from(err).to_code(), err.to_string().as_ref());
			}
			_ => {
				tracing::info!("session closed");
				self.session.close(SessionError::Cancel.to_code(), "");
			}
		}
		Poll::Ready(res)
	}

	fn poll_protocol(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut cx = Context::from_waker(waiter.waker());

		// Presenting tokens never ends the session.
		self.auth.poll(waiter);

		// The send-side machines never end the session; completion just retires them.
		if let Some(setup) = &mut self.setup
			&& setup.poll(&mut cx).is_ready()
		{
			self.setup = None;
		}
		if let Some(goaway) = &mut self.goaway
			&& goaway.poll(waiter).is_ready()
		{
			self.goaway = None;
		}

		if let Some(stream) = &mut self.session_stream
			&& let Poll::Ready(err) = poll_session_stream(stream, &mut cx)
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

impl<S: crate::transport::poll::Session> Drop for Driver<S> {
	fn drop(&mut self) {
		// Dropped without finishing: release anything still waiting on a token.
		self.auth.handle.close(Error::Cancel);
	}
}

/// Opens one AUTH stream per token this side presents.
struct Present<S: crate::transport::poll::Session> {
	runtime: crate::time::Clock,
	session: S,
	version: Version,
	handle: crate::auth::Handle,
	going_away: crate::goaway::GoingAway,
	tokens: kio::Tasks<PresentToken<S>>,
	/// Whether the first poll decided who answers the peer's tokens.
	started: bool,
	/// The connection's own credential, held for the life of the session.
	_setup: Option<crate::auth::Token>,
}

impl<S: crate::transport::poll::Session> Present<S> {
	fn poll(&mut self, waiter: &kio::Waiter) {
		if !self.started {
			// Decided once, before any AUTH stream can be accepted: the app took the
			// requests before running the driver, or the session answers itself.
			let _ = self.handle.acceptor();
			self.started = true;
		}
		while let Poll::Ready(Some((id, token))) = self.handle.poll_opening(waiter) {
			self.tokens.push(PresentToken {
				runtime: self.runtime.clone(),
				session: self.session.clone(),
				version: self.version,
				handle: self.handle.clone(),
				going_away: self.going_away.clone(),
				id,
				state: PresentState::Open { token },
			});
		}
		let _ = self.tokens.poll(waiter);
	}
}

/// One token's AUTH stream: send the token, then track the grant until either
/// side ends it.
struct PresentToken<S: crate::transport::poll::Session> {
	runtime: crate::time::Clock,
	session: S,
	version: Version,
	handle: crate::auth::Handle,
	going_away: crate::goaway::GoingAway,
	id: u64,
	state: PresentState<S>,
}

enum PresentState<S: crate::transport::poll::Session> {
	Open { token: bytes::Bytes },
	Run { stream: Stream<S, Version>, answered: bool },
	Done,
}

impl<S: crate::transport::poll::Session> kio::Task for PresentToken<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let err = ready!(self.poll_present(waiter));
		let answered = matches!(self.state, PresentState::Run { answered: true, .. });
		if let PresentState::Run { stream, .. } = std::mem::replace(&mut self.state, PresentState::Done) {
			stream.writer.abort(&Error::Cancel);
		}
		let err = match err {
			// A peer that predates AUTH resets a stream type it does not know, and one
			// that takes no tokens in band refuses the same way: either way, before any
			// reply, this token earns nothing here.
			Error::Stream(_) | Error::Decode(crate::DecodeError::Short) if !answered => Error::Unsupported,
			err => err,
		};
		match &err {
			Error::Cancel | Error::Unsupported | Error::Transport(_) | Error::Session(_) => {
				tracing::debug!(%err, "auth token ended")
			}
			err => tracing::warn!(%err, "auth token ended"),
		}
		self.handle.ended(self.id, err);
		Poll::Ready(())
	}
}

impl<S: crate::transport::poll::Session> PresentToken<S> {
	/// Run the stream, resolving with why the token ended.
	fn poll_present(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		let mut cx = Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				PresentState::Open { token } => {
					// After a GOAWAY the peer must not see new streams.
					if self.going_away.is_set() {
						return Poll::Ready(Error::GoingAway);
					}
					let token = token.clone();
					let mut stream = match ready!(Stream::poll_open(&mut self.session, self.version, &mut cx)) {
						Ok(stream) => stream,
						Err(err) => return Poll::Ready(err),
					};
					let res = stream
						.writer
						.buffer(&super::ControlType::Auth)
						.and_then(|()| stream.writer.buffer(&super::Auth { token }));
					if let Err(err) = res {
						return Poll::Ready(err);
					}
					self.state = PresentState::Run {
						stream,
						answered: false,
					};
				}
				PresentState::Run { stream, answered } => {
					if let Err(err) = ready!(stream.writer.poll_flush(&mut cx)) {
						return Poll::Ready(err);
					}
					// Withdrawn: closing the stream is what tells the peer.
					if self.handle.poll_withdrawn(self.id, waiter).is_ready() {
						return Poll::Ready(Error::Cancel);
					}
					let reply = match ready!(stream.reader.poll_decode_maybe::<super::AuthReply>(&mut cx)) {
						Ok(Some(reply)) => reply,
						// The peer ended the grant without revoking it, or closed without
						// ever answering (mapped to unsupported by the caller).
						Ok(None) if *answered => return Poll::Ready(Error::Cancel),
						Ok(None) => return Poll::Ready(Error::Decode(crate::DecodeError::Short)),
						Err(err) => return Poll::Ready(err),
					};
					match reply {
						super::AuthReply::Ok(ok) => {
							*answered = true;
							let now = crate::runtime::Timers::now(&self.runtime);
							self.handle.granted(
								self.id,
								crate::auth::Grant {
									publish: ok.publish,
									subscribe: ok.subscribe,
									expires: ok.expires.map(|expires| now + expires),
								},
							);
						}
						super::AuthReply::Error(refused) => {
							let code = u32::try_from(refused.code).unwrap_or(u32::MAX);
							let err = Error::Session(crate::SessionError::from_code(code));
							tracing::warn!(%err, reason = %refused.reason, "auth token refused");
							*answered = true;
							return Poll::Ready(err);
						}
					}
				}
				PresentState::Done => return Poll::Ready(Error::Cancel),
			}
		}
	}
}

/// Drain SessionInfo updates off the legacy session stream, resolving only when
/// the stream dies (a FIN counts: the peer abandoned the session).
// TODO do something useful with the updates
fn poll_session_stream<S: crate::transport::poll::Session>(
	stream: &mut Stream<S, Version>,
	cx: &mut Context<'_>,
) -> Poll<Error> {
	loop {
		match ready!(stream.reader.poll_decode_maybe::<SessionInfo>(cx)) {
			Ok(Some(_info)) => {}
			Ok(None) => return Poll::Ready(Error::Cancel),
			Err(err) => return Poll::Ready(err),
		}
	}
}

/// Advertise our capabilities on a uni Setup Stream, then FIN. Best-effort: an
/// error is logged and the machine finishes; the peer falls back to "no
/// capabilities" for us.
struct SendSetup<S: crate::transport::poll::Session> {
	version: Version,
	state: SendSetupState<S>,
}

enum SendSetupState<S: crate::transport::poll::Session> {
	/// Waiting for stream credit on our own session handle.
	Open {
		session: S,
		setup: Box<Setup>,
	},
	/// Flushing the buffered SETUP, then FIN and wait for the acknowledgement (a
	/// reset racing the FIN would discard the unacked message).
	Send {
		writer: Writer<S::SendStream, Version>,
		finished: bool,
	},
	Done,
}

impl<S: crate::transport::poll::Session> SendSetup<S> {
	fn new(session: S, setup: Setup, version: Version) -> Self {
		Self {
			version,
			state: SendSetupState::Open {
				session,
				setup: Box::new(setup),
			},
		}
	}

	fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		match ready!(self.poll_send(cx)) {
			Ok(()) => {}
			Err(err) => tracing::debug!(%err, "failed to send setup"),
		}
		self.state = SendSetupState::Done;
		Poll::Ready(())
	}

	fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				SendSetupState::Open { session, setup } => {
					let stream = ready!(session.poll_open_uni(cx)).map_err(Error::from_transport)?;
					let mut writer = Writer::new(stream, self.version);
					writer.buffer(&super::DataType::Setup)?;
					writer.buffer(&**setup)?;
					self.state = SendSetupState::Send {
						writer,
						finished: false,
					};
				}
				SendSetupState::Send { writer, finished } => {
					if !*finished {
						ready!(writer.poll_flush(cx))?;
						writer.finish()?;
						*finished = true;
					}
					return writer.poll_closed(cx);
				}
				SendSetupState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Send our single GOAWAY when the drain trigger fires, then enforce its local
/// deadline.
///
/// Runs on every version, including those with no GOAWAY message: the deadline is
/// the sender's own timer, so a caller draining a lite-03 peer still gets the
/// session closed on schedule; the peer just never learns why.
struct SendGoaway<S: crate::transport::poll::Session> {
	version: Version,
	runtime: crate::time::Clock,
	goaway: crate::goaway::Protocol,
	/// A dedicated handle for the trigger-phase close watch, since `session` opens
	/// the Goaway stream and each pending operation needs its own handle.
	closed: S,
	session: S,
	state: SendGoawayState<S>,
}

enum SendGoawayState<S: crate::transport::poll::Session> {
	/// Parked on the send trigger, racing the transport close so a parked trigger
	/// never blocks the driver. The trigger fires at most once.
	Waiting,
	/// Opening the Goaway control stream (0x5). Lite04+ only; earlier versions
	/// jump straight to enforcement.
	Open { payload: crate::goaway::Goaway },
	/// Flushing the single GOAWAY message, then FIN and wait for the
	/// acknowledgement before dropping: Writer's Drop resets the stream, and on
	/// real QUIC a reset racing the FIN discards the unacked GOAWAY frame.
	Send {
		stream: Stream<S, Version>,
		timeout: Option<std::time::Duration>,
		finished: bool,
	},
	/// The message is on the wire (or failed); enforce the local deadline.
	Enforce(crate::goaway::Enforce<S>),
}

impl<S: crate::transport::poll::Session> SendGoaway<S> {
	fn new(runtime: crate::time::Clock, session: S, goaway: crate::goaway::Protocol, version: Version) -> Self {
		Self {
			version,
			runtime,
			goaway,
			closed: session.clone(),
			session,
			state: SendGoawayState::Waiting,
		}
	}

	/// Move to enforcement, logging why the message never (fully) hit the wire.
	///
	/// Still enforce the deadline: the drain was requested, and failing to explain
	/// it to the peer is no reason to hold the session open.
	fn enforce_after(&mut self, err: Error, timeout: Option<std::time::Duration>) {
		tracing::warn!(%err, "failed to send goaway");
		self.state = SendGoawayState::Enforce(crate::goaway::Enforce::new(
			&self.runtime,
			self.session.clone(),
			timeout,
		));
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut cx = Context::from_waker(waiter.waker());
		loop {
			match &mut self.state {
				SendGoawayState::Waiting => {
					if self.closed.poll_closed(&mut cx).is_ready() {
						return Poll::Ready(());
					}
					let Some(payload) = ready!(self.goaway.poll_triggered(waiter)) else {
						return Poll::Ready(());
					};
					// moq-lite has no timeout field on the wire; only the URI is sent.
					// The deadline is still ours to honor, enforced locally below.
					self.state = if self.version.has_goaway() {
						SendGoawayState::Open { payload }
					} else {
						SendGoawayState::Enforce(crate::goaway::Enforce::new(
							&self.runtime,
							self.session.clone(),
							payload.timeout,
						))
					};
				}
				SendGoawayState::Open { payload } => {
					let timeout = payload.timeout;
					let mut stream = match ready!(Stream::poll_open(&mut self.session, self.version, &mut cx)) {
						Ok(stream) => stream,
						Err(err) => {
							self.enforce_after(err, timeout);
							continue;
						}
					};
					let msg = super::Goaway {
						uri: std::borrow::Cow::Borrowed(payload.uri.as_str()),
					};
					if let Err(err) = stream
						.writer
						.buffer(&super::ControlType::Goaway)
						.and_then(|()| stream.writer.buffer(&msg))
					{
						self.enforce_after(err, timeout);
						continue;
					}
					self.state = SendGoawayState::Send {
						stream,
						timeout,
						finished: false,
					};
				}
				SendGoawayState::Send {
					stream,
					timeout,
					finished,
				} => {
					let timeout = *timeout;
					let res = if !*finished {
						match ready!(stream.writer.poll_flush(&mut cx)) {
							Ok(()) => {
								*finished = true;
								stream.writer.finish()
							}
							Err(err) => Err(err),
						}
					} else {
						Ok(())
					};
					let res = match res {
						Ok(()) => ready!(stream.writer.poll_closed(&mut cx)),
						Err(err) => Err(err),
					};
					match res {
						Ok(()) => {
							self.state = SendGoawayState::Enforce(crate::goaway::Enforce::new(
								&self.runtime,
								self.session.clone(),
								timeout,
							));
						}
						Err(err) => self.enforce_after(err, timeout),
					}
				}
				SendGoawayState::Enforce(enforce) => return enforce.poll(waiter),
			}
		}
	}
}
