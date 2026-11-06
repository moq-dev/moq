use crate::driver::Transport;
use crate::origin;
use crate::transport::poll::Session as _;
use crate::{
	Error, Hop, SessionError, StreamError,
	coding::{Decode, DecodeError, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
	util::TaskSet,
};
use std::task::{Poll, ready};

use super::{
	Control, Message, Namespaces, PublishNamespaces, Publisher, RecvFill, RecvGroup, Subscriber, Version, active_count,
	auth, cluster, hidden, max_age, peer, request_stream::Reject, request_update, solicit,
	subscriber::is_protocol_violation,
};

type RecvStream = crate::transport::boxed::RecvStream;
type SendStream = crate::transport::boxed::SendStream;

/// Everything one moq-transport session needs to start.
pub struct Config {
	/// The runtime that arms the session's timers.
	pub runtime: crate::time::Deadlines,

	/// Where the session's control traffic is counted, even with neither origin set.
	pub stats: crate::stats::Session,

	pub session: Transport,

	/// What the peer may make this session hold.
	pub limits: crate::session::Limits,

	/// Whether we dialed, which sets the request-id parity.
	pub client: bool,

	/// Traffic stats are attributed through these origin handles: tag them with
	/// `origin::{Consumer, Producer}::with_stats` before calling [`start`].
	pub publish: Option<origin::Consumer>,
	pub subscribe: Option<origin::Producer>,

	/// The origin (hop) id to assign the peer when it declares none itself. See
	/// `Client::with_peer_hop`; a peer that negotiates the MoQ Cluster extension
	/// declares its own, which wins.
	pub peer_hop: Option<Hop>,

	/// What crossing this link costs. Declared in our SETUP (see
	/// [`cluster::RELAY_COST`]) for the peer to charge, and charged locally on what the
	/// peer sends us. `None` declares nothing and charges whatever the peer declared,
	/// falling back to 1; that is what a server accepting a connection passes, since it
	/// has no per-peer configuration of its own.
	///
	/// A peer that does not negotiate the MoQ Cluster extension carries no cost at all, so
	/// nothing is charged and its routes rank on hop count alone.
	pub cost: Option<u64>,

	pub version: Version,

	/// The request path we advertise in our SETUP (clients on URL-less
	/// transports). A server passes `None`.
	pub path: Option<String>,

	/// The URI authority we advertise in our SETUP, under the same rules as `path`.
	pub authority: Option<String>,

	/// The connection credential our SETUP's `AUTHORIZATION TOKEN` option presents. A
	/// server passes `None`.
	pub token: Option<crate::setup::Token>,

	/// The peer's SETUP stream, when it was already read before [`start`] (a server
	/// that gated on the client's path via [`SetupAccept`]). It becomes the
	/// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	pub peer_setup_stream: Option<Reader<RecvStream, crate::Version>>,

	/// What that pre-read SETUP declared, so the session does not have to parse it
	/// twice. `None` when [`Self::peer_setup_stream`] is.
	pub peer_declared: Option<peer::Peer>,

	/// The session's auth handle, created before [`start`] so a server can take the
	/// peer's token requests during its handshake. Supports AUTH exactly when the
	/// version can negotiate it and this side offers it; the peer's SETUP decides whether
	/// it does.
	pub auth: crate::auth::Link,

	/// Uni streams that arrived before that pre-read SETUP (see [`PeerSetup::early`]),
	/// classified by the session before it accepts any more.
	pub early_unis: Vec<Reader<RecvStream, crate::Version>>,

	/// The extensions our SETUP offers. MoQ Auth is offered only while [`Self::auth`]
	/// supports it too.
	pub extensions: crate::setup::Extensions,

	/// The Implementation our SETUP advertises, or `None` to omit it.
	pub implementation: Option<String>,

	/// Where the implementation the peer's SETUP advertises is published, once the uni loop
	/// reads it. A caller that pre-read the SETUP has already filled it.
	pub peer_implementation: crate::setup::PeerImplementation,
}

pub(crate) struct Driver {
	pub(crate) withdrawal: crate::session::Withdrawal,
	// Dispatched SUBSCRIBE and FETCH serves still owing the peer data.
	owed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
	machine: Machine,
	/// Keeps the test executor's registrations alive between polls of the future form.
	#[cfg(test)]
	park: Box<kio::Park>,
}

impl Driver {
	/// Whether withdrawals and dispatched serves have reached the peer.
	pub(crate) fn drained(&self) -> bool {
		self.withdrawal.drained() && self.owed.load(std::sync::atomic::Ordering::Relaxed) == 0
	}

	/// Run the session until it ends, with the error that ended it.
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.machine.poll(waiter)
	}
}

#[cfg(test)]
impl std::future::Future for Driver {
	type Output = Result<(), Error>;
	fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
		let Self { machine, park, .. } = &mut *self;
		machine.poll(park.hold(cx))
	}
}

/// Where the session is in its life.
enum Machine {
	/// Not polled yet. The session is built on the first poll, since until then the app
	/// may still take the peer's Auth requests.
	Start(Box<Launch>),
	Running(Box<Running>),
	Done,
}

impl Machine {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if matches!(self, Self::Start(_)) {
			let Self::Start(launch) = std::mem::replace(self, Self::Done) else {
				unreachable!("matched above");
			};
			*self = Self::Running(Box::new(launch.run()));
		}
		let Self::Running(running) = self else {
			panic!("session driver polled after it finished");
		};
		let res = ready!(running.poll(waiter));
		let Self::Running(running) = std::mem::replace(self, Self::Done) else {
			unreachable!("matched above");
		};
		running.finish(&res);
		Poll::Ready(res)
	}
}

pub fn start(config: Config) -> Result<(Driver, crate::goaway::Handle, Option<crate::auth::Credential>), Error> {
	let Config {
		runtime,
		stats,
		session,
		limits,
		client,
		publish,
		subscribe,
		peer_hop,
		cost,
		version,
		path,
		authority,
		token,
		peer_setup_stream,
		peer_declared,
		auth,
		early_unis,
		extensions,
		implementation,
		peer_implementation,
	} = config;

	// GOAWAY wiring: the public Session holds one half (drain trigger, received
	// signal), the protocol tasks below hold the other.
	// A moq-transport client MUST send an empty New Session URI: it cannot tell a
	// server to open connections (draft-22 section 9.2).
	let (goaway_handle, goaway) = crate::goaway::Handle::new(!client);

	// What the peer's connection credential earns by default, from the caller's real
	// handles before the empty-half defaulting below.
	let peer_grant = auth::peer_grant(publish.as_ref(), subscribe.as_ref());

	// Present the connection's own credential (the empty token) right away, so both
	// sides learn their grant without waiting on the app. The peer's SETUP then decides
	// whether it is ever sent.
	// A handle that already knows the peer declined (a gated server accept) skips it.
	let setup_token = match auth.supported() {
		true => auth.present(bytes::Bytes::new(), true).ok(),
		false => None,
	};

	// Which prefixes to SUBSCRIBE_NAMESPACE, as for moq-lite: a client asks about what
	// its origin's readers watch, a server or relay link about its whole scope. Decided
	// now, so the origin's announce consumers never read as synced before a request goes
	// out. An unset half asks nothing.
	let discovery = subscribe
		.as_ref()
		.map(|origin| crate::discovery::Discovery::new(origin, client, runtime.clone()));

	let withdrawal = crate::session::Withdrawal::default();
	let owed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

	// What the peer declared in its SETUP. Seeded now when that stream was already
	// read (the legacy handshake, or a gated server accept), and filled by the uni
	// loop otherwise.
	let mut peer_setup = peer::PeerSetup::default().with_implementation(peer_implementation);
	let setup_read = peer_declared.is_some();
	match peer_declared {
		Some(declared) => peer_setup.set(declared),
		// A client is live before the server's SETUP, which follows its check of the
		// credential.
		None if client => peer_setup = peer_setup.accepting(auth.clone()),
		None => {}
	}

	let credential = setup_token.clone();
	let launch = Launch {
		setup_token,
		auth,
		runtime,
		stats,
		session,
		limits,
		client,
		publish,
		subscribe,
		peer_hop,
		cost,
		version,
		setup: SetupParams {
			path,
			authority,
			token,
			implementation,
			extensions,
		},
		peer_setup_stream,
		early_unis,
		goaway,
		peer_grant,
		discovery,
		withdrawal: withdrawal.clone(),
		owed: owed.clone(),
		peer_setup,
		setup_read,
	};

	Ok((
		Driver {
			withdrawal,
			owed,
			machine: Machine::Start(Box::new(launch)),
			#[cfg(test)]
			park: Default::default(),
		},
		goaway_handle,
		credential,
	))
}

/// What [`start`] hands the session to build on its first poll.
struct Launch {
	setup_token: Option<crate::auth::Credential>,
	auth: crate::auth::Link,
	runtime: crate::time::Deadlines,
	stats: crate::stats::Session,
	session: Transport,
	limits: crate::session::Limits,
	client: bool,
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	peer_hop: Option<Hop>,
	cost: Option<u64>,
	version: Version,
	setup: SetupParams,
	peer_setup_stream: Option<Reader<RecvStream, crate::Version>>,
	early_unis: Vec<Reader<RecvStream, crate::Version>>,
	goaway: crate::goaway::Protocol,
	peer_grant: crate::auth::Grant,
	discovery: Option<crate::discovery::Discovery>,
	withdrawal: crate::session::Withdrawal,
	owed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
	peer_setup: peer::PeerSetup,
	setup_read: bool,
}

impl Launch {
	fn run(self) -> Running {
		let Self {
			setup_token,
			auth,
			runtime,
			stats,
			session,
			limits,
			client,
			publish,
			subscribe,
			peer_hop,
			cost,
			version,
			setup,
			peer_setup_stream,
			early_unis,
			goaway,
			peer_grant,
			discovery,
			withdrawal,
			owed,
			peer_setup,
			setup_read,
		} = self;

		// Released on any exit, so nothing waits on a token the session will never answer.
		let auth_close = AuthClose(auth.clone());
		// Decided once, before any Auth request can be accepted: the app took the requests
		// before running the driver, or the session answers itself.
		let _ = auth.acceptor();
		// Our own Hop ID, taken from whichever origin the caller actually supplied so
		// every session out of this process stamps the same one and cross-session loop
		// detection works. Read BEFORE the placeholders below: their ids are random and
		// identify nothing, so declaring one would compare incoming paths against an
		// identity no other session shares.
		let self_origin = self_origin(publish.as_ref(), subscribe.as_ref());

		// moq-transport threads concrete origins through the publisher/subscriber.
		// An unset half gets an empty origin: an empty publish origin announces
		// nothing, and an empty subscribe origin issues no SUBSCRIBE_NAMESPACE.
		let publish = publish.unwrap_or_else(|| {
			origin::Producer::empty(Hop::random())
				.consume()
				.with_stats(stats.clone())
		});
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Hop::random()).with_stats(stats.clone()));
		// Counted for the peak only: stream credit bounds them.
		let subscriptions =
			crate::session::Slots::default().with_stats(publish.stats(), crate::stats::Cap::Subscriptions);
		let announces =
			crate::session::Slots::new(limits.announces).with_stats(subscribe.stats(), crate::stats::Cap::Announces);

		let control = Control::new(client);
		// Send SETUP and retain its writer for the session's lifetime. Only a failure
		// ends the session; the other halves report how it closed.
		let send_setup = SendSetup::new(
			session.clone(),
			version,
			SetupParams {
				extensions: crate::setup::Extensions {
					auth: setup.extensions.auth && auth.supported(),
					..setup.extensions
				},
				..setup
			},
			self_origin,
			cost,
			goaway.clone(),
			control.clone(),
			stats.clone(),
		);

		let mut publisher = Publisher::new(
			runtime.clone(),
			session.clone(),
			publish,
			control.clone(),
			peer_hop,
			peer_setup.clone(),
			version,
		)
		.with_auth(auth.clone());
		let (tasks, task_set) = TaskSet::new();
		publisher.withdrawal = withdrawal;
		publisher.owed = owed;
		publisher.subscriptions = subscriptions;

		// Only the dialing side fails loud on a publication outside its grant: a
		// server's publish origin is everything the peer may read, not what it
		// intends to push.
		let enforce = client.then(|| EnforceGrant::new(auth.clone(), publisher.clone(), session.clone()));

		let mut subscriber = Subscriber::new(
			runtime.clone(),
			session.clone(),
			subscribe,
			control.clone(),
			peer_hop,
			peer_setup.clone(),
			self_origin,
			cost,
			version,
			tasks,
			goaway.going_away.clone(),
		)
		.with_auth(auth.clone())
		.with_solicit(setup.extensions.solicit);

		// Our tokens, one Auth request each, once the peer's SETUP negotiates it.
		let present = auth::Presenting::new(
			runtime.clone(),
			session.clone(),
			control.clone(),
			auth.clone(),
			peer_setup.clone(),
			version,
			stats.clone(),
		);
		// Only a side that offered MoQ Auth answers the peer's Auth requests: one that
		// arrives otherwise falls through to the protocol violation in `Dispatch`.
		let serve = auth.supported().then(|| auth::Serve {
			runtime: runtime.clone(),
			handle: auth.clone(),
			peer_grant,
		});
		subscriber.announces = announces;

		// When the peer's SETUP was pre-read (a gated server accept), monitor GOAWAY on
		// that stream here; otherwise `Unis` does it when the SETUP arrives on the wire.
		let goaway_recv = peer_setup_stream.map(|reader| {
			ReadGoaways::new(
				reader.with_stats(&stats).with_version(version),
				version,
				client,
				goaway.clone(),
			)
		});

		let unis = Unis::new(
			session.clone(),
			subscriber.clone(),
			UniSetup {
				peer: Some(peer_setup.clone()),
				read: setup_read,
				early: early_unis,
				client,
				version,
				goaway,
			},
		);
		let publish_namespaces = PublishNamespaces::new(publisher.clone());
		let dispatch = Dispatch::new(
			session.clone(),
			publisher,
			subscriber.clone(),
			peer_setup,
			serve,
			version,
		);
		// A transport without datagrams never delivers one.
		let datagrams = (session.max_datagram_size() != 0).then(|| Datagrams {
			session: session.clone(),
			subscriber: subscriber.clone(),
			budget: kio::coop::Budget::new(32),
			charged: false,
		});
		// Keep the session's SUBSCRIBE_NAMESPACE requests open as discovery decides, each on
		// its own stream; with no subscribe half, ask nothing.
		let namespaces = discovery.map(|discovery| Namespaces::new(subscriber.clone(), discovery));

		Running {
			race: Race {
				namespaces,
				publish_namespaces: Some(publish_namespaces),
				setup: Some(send_setup),
				enforce,
				present: Some(present),
				goaway: goaway_recv,
				datagrams,
				dispatch: Some(dispatch),
				unis,
				subscriber,
				tasks: task_set,
				session,
			},
			ending: None,
			auth,
			auth_close,
			setup_token,
		}
	}
}

/// A running session: its halves race until one fails or the task set drains.
struct Running {
	race: Race,
	/// How the race ended, held while what already arrived drains.
	ending: Option<Result<(), Error>>,
	auth: crate::auth::Link,
	auth_close: AuthClose,
	/// Held for the life of the session.
	setup_token: Option<crate::auth::Credential>,
}

impl Running {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.ending.is_none() {
			let res = ready!(self.race.poll(waiter));
			close(&mut self.race.session, &res);
			// Before the Auth serve tasks drop, so each settles with the session's error
			// rather than a bare cancel.
			self.auth.close(match &res {
				Ok(()) => Error::Cancel,
				Err(err) => err.clone(),
			});
			self.ending = Some(res);
		}
		// Whichever side closed it, read what already arrived.
		ready!(self.race.unis.poll_drain(waiter));
		let res = self.ending.take().expect("draining an ended session");
		// Closed cleanly by dropping or draining: readers see what the peer's clean
		// close would have shown them.
		if self.race.session.closing().is_local() {
			self.race.subscriber.abort(&Error::Session(SessionError::Cancel));
		} else if let Err(err) = &res {
			// Every track this session was receiving ends with its error.
			self.race.subscriber.abort(err);
		}
		Poll::Ready(res)
	}

	/// Drop the halves, then release the auth handle and our connection credential.
	fn finish(self: Box<Self>, res: &Result<(), Error>) {
		let Self {
			race,
			auth,
			auth_close,
			setup_token,
			..
		} = *self;
		drop(race);
		auth.close(match res {
			Ok(()) => Error::Cancel,
			Err(err) => err.clone(),
		});
		drop(auth_close);
		drop(setup_token);
	}
}

/// The halves of a running session. Every half but the task set ends the session only by
/// failing: one that finishes cleanly drops out, and the task set draining is the one clean
/// exit. Fields drop in declaration order: the halves in the reverse of their poll order,
/// then the uni streams, the subscriber and the task set.
struct Race {
	/// Our SUBSCRIBE_NAMESPACE requests; `None` without a subscribe half.
	namespaces: Option<Namespaces>,
	/// Unsolicited PUBLISH_NAMESPACE unless the peer requires solicitation.
	publish_namespaces: Option<PublishNamespaces>,
	/// Never finishes while the session lives: it holds our SETUP stream open.
	setup: Option<SendSetup>,
	/// `None` on a server, which publishes whatever the peer may read.
	enforce: Option<EnforceGrant>,
	present: Option<auth::Presenting>,
	/// GOAWAY on a pre-read SETUP stream; `None` when `unis` reads the SETUP.
	goaway: Option<ReadGoaways<RecvStream>>,
	datagrams: Option<Datagrams>,
	dispatch: Option<Dispatch>,
	unis: Unis,
	subscriber: Subscriber,
	tasks: TaskSet,
	session: Transport,
}

impl Race {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if let Poll::Ready(res) = self.unis.poll(waiter) {
			return Poll::Ready(res);
		}
		if let Poll::Ready(err) = poll_failed(&mut self.dispatch, |dispatch| dispatch.poll(waiter).map(Err)) {
			return Poll::Ready(Err(err));
		}
		if let Poll::Ready(err) = poll_failed(&mut self.datagrams, |datagrams| datagrams.poll(waiter)) {
			return Poll::Ready(Err(err));
		}
		if let Poll::Ready(err) = poll_failed(&mut self.goaway, |goaway| {
			goaway.poll(waiter).map(|err| Err(control_stream_error(err)))
		}) {
			return Poll::Ready(Err(err));
		}
		// Malformed replies are session-fatal, unlike token refusals.
		if let Poll::Ready(err) = poll_failed(&mut self.present, |present| present.poll(waiter)) {
			return Poll::Ready(Err(err));
		}
		if let Poll::Ready(err) = poll_failed(&mut self.enforce, |enforce| enforce.poll(waiter)) {
			return Poll::Ready(Err(err));
		}
		if let Poll::Ready(err) = poll_failed(&mut self.setup, |setup| setup.poll(waiter)) {
			return Poll::Ready(Err(err));
		}
		if self.tasks.poll(waiter).is_ready() {
			return Poll::Ready(Ok(()));
		}
		if let Poll::Ready(err) = poll_failed(&mut self.namespaces, |namespaces| namespaces.poll(waiter).map(Err)) {
			return Poll::Ready(Err(err));
		}
		if let Poll::Ready(err) = poll_failed(&mut self.publish_namespaces, |publish| publish.poll(waiter)) {
			return Poll::Ready(Err(err));
		}
		Poll::Pending
	}
}

/// Poll a half of the race, `Ready` only with its failure. A half that finishes, either
/// way, drops right away and is never polled again.
fn poll_failed<T>(half: &mut Option<T>, poll: impl FnOnce(&mut T) -> Poll<Result<(), Error>>) -> Poll<Error> {
	let Some(running) = half else {
		return Poll::Pending;
	};
	let res = ready!(poll(running));
	*half = None;
	match res {
		Ok(()) => Poll::Pending,
		Err(err) => Poll::Ready(err),
	}
}

/// Close the session over the driver's result, while its streams are still open.
///
/// Dropping them first would reset the control stream ahead of the close, and a strict peer
/// closes over that reset with its own PROTOCOL_VIOLATION before it reads our code.
fn close(session: &mut Transport, res: &Result<(), Error>) {
	match res {
		Err(err @ (Error::Transport(_) | Error::IdleTimeout)) => {
			tracing::info!(%err, "session terminated");
			session.abort(err, super::error::to_session_code(&SessionError::Internal), "");
		}
		Err(err) => {
			tracing::warn!(%err, "session error");
			session.abort(
				err,
				super::error::to_session_code(&SessionError::from(err)),
				err.to_string().as_ref(),
			);
		}
		Ok(()) => {
			tracing::info!("session closed");
			session.abort(&Error::Cancel, super::error::to_session_code(&SessionError::Cancel), "");
		}
	}
}

/// What a peer's SETUP told us, beyond the stream it arrived on.
pub struct PeerSetup {
	/// The SETUP stream, which becomes the GOAWAY channel.
	pub stream: Reader<RecvStream, crate::Version>,

	/// The SETUP's encoded size, counted once the session's stats are known.
	pub size: usize,

	/// What the SETUP's parameters say.
	pub options: SetupOptions,

	/// Uni streams that arrived before the SETUP, type peeked but unread. The drafts say
	/// to buffer early data until the control streams arrive, so the session classifies
	/// these once it starts. QUIC stream credit bounds how many can pile up.
	pub early: Vec<Reader<RecvStream, crate::Version>>,
}

/// The Hop ID this session declares and detects loops against.
///
/// Both halves of a session share the process's origin identity, so either one names
/// it; the publish half is just the usual one to be set. A session with neither half
/// has no content to route, so a throwaway id is all it can offer.
fn self_origin(publish: Option<&origin::Consumer>, subscribe: Option<&origin::Producer>) -> Hop {
	publish
		.map(|origin| origin.hop())
		.or_else(|| subscribe.map(|origin| origin.hop()))
		.unwrap_or_else(Hop::random)
}

/// Server: reads the peer's SETUP off its uni stream before starting the
/// session, resolving to that stream plus what it declared.
///
/// Waits for the peer's Setup Stream. Any other uni stream racing ahead of it (padding,
/// or group data for a subscription the peer already holds) is held in
/// [`PeerSetup::early`] for the session to classify. Pass the returned reader to
/// [`start`] as its `peer_setup_stream` so GOAWAY monitoring continues without
/// re-reading it.
pub struct SetupAccept {
	version: Version,
	// Each stream racing ahead of SETUP is held, so a peer opening them at once must not
	// hold the task for its whole MAX_STREAMS credit.
	budget: kio::coop::Budget,
	early: Vec<Reader<crate::transport::boxed::RecvStream, crate::Version>>,
	step: SetupStep,
}

/// Where [`SetupAccept`] waits. Streams are taken one at a time: the next is accepted only
/// once the last one's type has arrived.
enum SetupStep {
	/// Spending a pass of the budget before accepting another stream.
	Yield,
	/// Waiting for the peer's next uni stream.
	Accept,
	/// Waiting for this stream's type.
	Peek(Reader<crate::transport::boxed::RecvStream, crate::Version>),
	/// Waiting for the whole SETUP on its stream.
	Decode(Reader<crate::transport::boxed::RecvStream, crate::Version>),
}

impl SetupAccept {
	/// Start waiting for a `version` peer's SETUP.
	pub fn new(version: Version) -> Self {
		Self {
			version,
			budget: kio::coop::Budget::new(32),
			early: Vec::new(),
			step: SetupStep::Yield,
		}
	}

	/// The peer's SETUP, with the streams that raced ahead of it.
	pub fn poll(&mut self, session: &Transport, waiter: &kio::Waiter) -> Poll<Result<PeerSetup, Error>> {
		loop {
			match &mut self.step {
				SetupStep::Yield => {
					ready!(self.budget.poll_yield(waiter));
					self.step = SetupStep::Accept;
				}
				SetupStep::Accept => {
					let recv = ready!(session.poll_accept_uni(waiter)).map_err(Error::from_transport)?;
					self.step = SetupStep::Peek(Reader::new(recv, crate::Version::Ietf(self.version)));
				}
				SetupStep::Peek(reader) => {
					let kind = match ready!(reader.poll_varint_peek(waiter)) {
						Ok(kind) => kind,
						Err(err) if died_before_header(&err) => {
							tracing::debug!(%err, "dropping uni stream that died before its type");
							self.step = SetupStep::Yield;
							continue;
						}
						Err(err) => return Poll::Ready(Err(err)),
					};
					let SetupStep::Peek(mut reader) = std::mem::replace(&mut self.step, SetupStep::Yield) else {
						unreachable!("matched above");
					};
					if kind != setup::SETUP {
						self.early.push(reader);
						continue;
					}
					reader.set_priority(crate::priority::RECV_CONTROL);
					self.step = SetupStep::Decode(reader);
				}
				SetupStep::Decode(reader) => {
					let (setup, size) = ready!(reader.poll_decode_sized::<setup::Setup>(waiter))?;
					let options = decode_peer_setup(&setup.parameters, self.version, false)?;
					if session.address() == crate::transport::Address::Uri && options.path.is_some() {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let SetupStep::Decode(stream) = std::mem::replace(&mut self.step, SetupStep::Yield) else {
						unreachable!("matched above");
					};
					return Poll::Ready(Ok(PeerSetup {
						stream,
						size,
						options,
						early: std::mem::take(&mut self.early),
					}));
				}
			}
		}
	}
}

/// [`SetupAccept`] as a future, for tests.
#[cfg(test)]
pub(crate) async fn accept_setup(session: &Transport, version: Version) -> Result<PeerSetup, Error> {
	let mut accept = SetupAccept::new(version);
	kio::wait(|waiter| accept.poll(session, waiter)).await
}

/// What a peer's SETUP parameters say, parsed by [`decode_peer_setup`] for every caller.
pub struct SetupOptions {
	/// The request path the peer advertised, for URL-less transports.
	pub path: Option<String>,

	/// The credential the peer presented in its `AUTHORIZATION TOKEN` option.
	pub token: Option<crate::setup::Token>,

	/// The implementation the peer advertised in its `IMPLEMENTATION` option.
	pub implementation: Option<String>,

	/// The Setup Options it declared (see [`cluster`] and [`solicit`]).
	pub declared: peer::Peer,
}

/// Parse a peer's raw SETUP parameter block, refusing any option the peer's role may not
/// send. `client` is our role, so `true` means the block came from a server.
pub(crate) fn decode_peer_setup(parameters: &[u8], version: Version, client: bool) -> Result<SetupOptions, Error> {
	let (params, _) = ietf::Parameters::decode_slice(parameters, version)?;
	let path = params.get_bytes(ietf::ParameterBytes::Path);
	// A server sends neither PATH nor AUTHORITY (draft-22 sections 9.1.1 and 9.1.2).
	if client && path.is_some() {
		return Err(SessionError::InvalidPath.into());
	}
	if client && params.get_bytes(ietf::ParameterBytes::Authority).is_some() {
		return Err(SessionError::InvalidAuthority.into());
	}
	let path = match path {
		Some(bytes) => Some(
			std::str::from_utf8(bytes)
				.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
				.to_owned(),
		),
		None => None,
	};

	Ok(SetupOptions {
		path,
		// A token that cannot be decoded closes the session even where nothing uses it
		// (draft-22 section 8.9).
		token: super::token::from_setup(&params, version)?,
		implementation: crate::setup::decode_implementation(params.get_bytes(ietf::ParameterBytes::Implementation))?,
		declared: peer::Peer {
			cluster: cluster::peer_from_setup(&params)?,
			solicit: solicit::from_setup(&params, version)?,
			hidden: hidden::from_setup(&params, version),
			max_age: max_age::from_setup(&params, version),
			auth: auth::from_setup(&params) == Some(true),
			active_count: active_count::from_setup(&params),
			flate: super::flate::from_setup(&params)?,
		},
	})
}

/// What our SETUP declares beyond the MoQ Cluster options.
struct SetupParams {
	/// The request path we advertise (clients on URL-less transports); a server passes `None`.
	path: Option<String>,
	authority: Option<String>,
	/// The client's connection credential; a server passes `None`.
	token: Option<crate::setup::Token>,
	implementation: Option<String>,
	/// The other extensions we offer.
	extensions: crate::setup::Extensions,
}

/// Sends our SETUP on a uni stream and keeps it alive: this stream is also our GOAWAY
/// channel, so a fired drain trigger encodes the GOAWAY here.
///
/// `self_origin` and `cost` are the MoQ Cluster options, which declare our identity and
/// (client-only) what this link costs to cross.
struct SendSetup {
	session: Transport,
	version: Version,
	self_origin: Hop,
	cost: Option<u64>,
	goaway: crate::goaway::Protocol,
	control: Control,
	stats: crate::stats::Session,
	step: SetupSend,
}

/// Where [`SendSetup`] waits.
enum SetupSend {
	/// Waiting for credit to open the stream.
	Open(crate::transport::open::Open, SetupParams),
	/// Flushing the SETUP.
	Send(Writer<SendStream, crate::Version>),
	/// Holding the stream until the session closes or the drain trigger fires.
	Hold {
		writer: Writer<SendStream, crate::Version>,
		/// The trigger resolved without a GOAWAY: the session dropped without draining.
		untriggered: bool,
	},
	/// Flushing our GOAWAY.
	GoAway(Writer<SendStream, Version>),
	/// Holding the stream after our GOAWAY until the session closes.
	Linger(Writer<SendStream, Version>),
	/// Between steps.
	Done,
}

impl SendSetup {
	#[expect(
		clippy::too_many_arguments,
		reason = "private plumbing; a single-use argument struct would only rename the parameters"
	)]
	fn new(
		session: Transport,
		version: Version,
		params: SetupParams,
		self_origin: Hop,
		cost: Option<u64>,
		goaway: crate::goaway::Protocol,
		control: Control,
		stats: crate::stats::Session,
	) -> Self {
		Self {
			session,
			version,
			self_origin,
			cost,
			goaway,
			control,
			stats,
			step: SetupSend::Open(Default::default(), params),
		}
	}

	/// Encode what our SETUP declares.
	fn parameters(&self, params: SetupParams) -> Result<bytes::Bytes, Error> {
		let version = self.version;
		let mut parameters = ietf::Parameters::default();
		if let Some(implementation) = params.implementation {
			parameters.set_bytes(ietf::ParameterBytes::Implementation, implementation.into_bytes());
		}
		if let Some(path) = params.path {
			parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
		}
		if let Some(authority) = params.authority {
			parameters.set_bytes(ietf::ParameterBytes::Authority, authority.into_bytes());
		}
		if let Some(token) = &params.token {
			super::token::into_setup(&mut parameters, token, version)?;
		}
		cluster::peer_into_setup(&mut parameters, self.self_origin, self.cost);
		if params.extensions.solicit {
			solicit::into_setup(&mut parameters, version);
		}
		hidden::into_setup(&mut parameters, version);
		max_age::into_setup(&mut parameters, version);
		if params.extensions.auth {
			auth::into_setup(&mut parameters);
		}
		active_count::into_setup(&mut parameters);
		request_update::into_setup(&mut parameters, version);
		super::flate::into_setup(&mut parameters);
		Ok(parameters.encode_bytes(version)?)
	}

	/// `Ready` once the session closed with the stream held, or with why it broke.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.step {
				SetupSend::Open(open, _) => {
					let send = ready!(self.session.poll_open_uni(open, waiter, Default::default()))
						.map_err(Error::from_transport)?;
					let SetupSend::Open(_, params) = std::mem::replace(&mut self.step, SetupSend::Done) else {
						unreachable!("matched above");
					};
					let mut writer: Writer<SendStream, crate::Version> =
						Writer::new(send, crate::Version::Ietf(self.version)).with_stats(&self.stats);
					let parameters = self.parameters(params)?;
					writer
						.buffer(&setup::Setup { parameters })
						.map_err(control_stream_error)?;
					self.step = SetupSend::Send(writer);
				}
				SetupSend::Send(writer) => {
					ready!(writer.poll_flush(waiter)).map_err(control_stream_error)?;
					let SetupSend::Send(writer) = std::mem::replace(&mut self.step, SetupSend::Done) else {
						unreachable!("matched above");
					};
					self.step = SetupSend::Hold {
						writer,
						untriggered: false,
					};
				}
				// Hold the writer alive until the session closes, sending a GOAWAY if the
				// drain trigger fires meanwhile. The trigger resolves `None` when the session
				// drops without draining; keep holding either way, since a FIN mid-session is
				// a protocol violation the peer closes over.
				SetupSend::Hold { writer, untriggered } => {
					if self.session.poll_closed(waiter).is_ready() {
						writer.finish().ok();
						return Poll::Ready(Ok(()));
					}
					if writer.poll_closed(waiter).is_ready() {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					if *untriggered {
						return Poll::Pending;
					}
					let payload = match ready!(self.goaway.poll_triggered(waiter)) {
						Some(payload) => payload,
						None => {
							*untriggered = true;
							return Poll::Pending;
						}
					};
					let SetupSend::Hold { writer, .. } = std::mem::replace(&mut self.step, SetupSend::Done) else {
						unreachable!("matched above");
					};

					let timeout_ms = payload.timeout.map(|d| d.as_millis() as u64).unwrap_or(0);
					let msg = ietf::GoAway {
						new_session_uri: std::borrow::Cow::Borrowed(payload.uri.as_ref()),
						timeout: timeout_ms,
						request_id: (self.version == Version::Draft18)
							.then(|| self.control.goaway())
							.transpose()?,
					};

					// Frame as [type_id varint][size u16][body], the same shape as the
					// control-stream messages this channel otherwise carries.
					let mut writer = writer.with_version(self.version);
					writer
						.buffer_message(ietf::GoAway::ID, &msg)
						.map_err(control_stream_error)?;
					self.step = SetupSend::GoAway(writer);
				}
				SetupSend::GoAway(writer) => {
					ready!(writer.poll_flush(waiter)).map_err(control_stream_error)?;
					let SetupSend::GoAway(writer) = std::mem::replace(&mut self.step, SetupSend::Done) else {
						unreachable!("matched above");
					};
					self.step = SetupSend::Linger(writer);
				}
				SetupSend::Linger(writer) => {
					if self.session.poll_closed(waiter).is_ready() {
						writer.finish().ok();
						return Poll::Ready(Ok(()));
					}
					if writer.poll_closed(waiter).is_ready() {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					return Poll::Pending;
				}
				SetupSend::Done => unreachable!("polled after it finished"),
			}
		}
	}
}

/// The PADDING stream type (draft-18+): bytes a peer sends to probe for bandwidth.
const PADDING: u64 = 0x132B3E28;

/// What a unidirectional stream's type names on the negotiated draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UniType {
	/// The peer's SETUP, which then carries its GOAWAY.
	Setup,
	/// A SUBGROUP_HEADER, carrying one subgroup of a subscription.
	Subgroup,
	/// A FETCH_HEADER, carrying a fetch response.
	Fetch,
	/// Data to discard (draft-18+).
	Padding,
}

impl UniType {
	/// `None` is a type the draft does not define, which MUST close the session
	/// (draft-22 section 6.4.1, and its equivalent in every draft we negotiate).
	fn classify(kind: u64) -> Option<Self> {
		// SUBGROUP_HEADER types are 0x10-0x1D and 0x30-0x3D, plus 0x40 (FIRST_OBJECT)
		// covering 0x50-0x5D and 0x70-0x7D. A reserved
		// SUBGROUP_ID_MODE (0b11) or a bit the draft lacks is invalid, which MUST close the
		// session too (draft-22 section 11.3.1).
		if ietf::GroupFlags::decode(kind).is_ok() {
			return Some(Self::Subgroup);
		}

		match kind {
			FetchHeader::TYPE => Some(Self::Fetch),
			setup::SETUP => Some(Self::Setup),
			PADDING => Some(Self::Padding),
			_ => None,
		}
	}
}

/// Setup and lifecycle state for the incoming uni stream dispatcher.
struct UniSetup {
	peer: Option<peer::PeerSetup>,
	read: bool,
	/// Streams accepted before the session started, handled before accepting more.
	early: Vec<Reader<RecvStream, crate::Version>>,
	/// Our role, which decides what the peer's SETUP and GOAWAY may carry.
	client: bool,
	version: Version,
	goaway: crate::goaway::Protocol,
}

/// Accept incoming uni streams, including SETUP and GOAWAY.
///
/// Each stream reads its own type in its own child. QUIC opens every lower-numbered
/// stream once a higher one arrives, so the next stream accepted can be one whose bytes
/// the peer has not sent yet, held back by connection flow control that the later
/// streams' unread data is using up. Waiting here for its type would deadlock the
/// connection.
struct Unis {
	session: Transport,
	uni: Uni,
	early: std::collections::VecDeque<Reader<RecvStream, crate::Version>>,
	tasks: TaskSet,
	fatal: kio::Shared<Option<Error>>,
	ended: Option<Error>,
}

impl Unis {
	fn new(session: Transport, subscriber: Subscriber, setup: UniSetup) -> Self {
		Self {
			uni: Uni {
				subscriber,
				peer_setup: setup.peer,
				seen_setup: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(setup.read)),
				goaway: setup.goaway,
				client: setup.client,
				version: setup.version,
			},
			session,
			early: setup.early.into(),
			tasks: TaskSet::owned(),
			fatal: Default::default(),
			ended: None,
		}
	}

	fn accept(&mut self, waiter: &kio::Waiter) {
		// MAX_STREAMS credit alone lets one poll accept thousands of streams, so a peer
		// opening them at once must not hold the session's other tasks. A yield has woken
		// the task, and both callers end Pending while the session lives.
		let mut budget = kio::coop::Budget::new(32);
		while self.ended.is_none() && budget.poll_yield(waiter).is_ready() {
			let reader = match self.early.pop_front() {
				Some(reader) => reader,
				None => match self.session.poll_accept_uni(waiter) {
					Poll::Ready(Ok(recv)) => Reader::new(recv, crate::Version::Ietf(self.uni.version)),
					Poll::Ready(Err(err)) => {
						self.ended = Some(Error::from_transport(err));
						break;
					}
					Poll::Pending => break,
				},
			};
			self.tasks
				.push(UniServe::new(self.uni.clone(), reader, self.fatal.clone()));
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let _ = self.tasks.poll(waiter);
		if let Some(err) = self.fatal.lock().take() {
			return Poll::Ready(Err(err));
		}
		self.accept(waiter);
		let _ = self.tasks.poll(waiter);
		if let Some(err) = self.fatal.lock().take() {
			return Poll::Ready(Err(err));
		}
		match &self.ended {
			Some(err) => Poll::Ready(Err(err.clone())),
			None => Poll::Pending,
		}
	}

	fn poll_drain(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		self.accept(waiter);
		let drained = self.tasks.poll(waiter).is_ready();
		if self.ended.is_none() {
			return Poll::Pending;
		}
		// Every accepted child has first consumed its buffered header. A fill
		// still waiting to be promised or opened can no longer arrive.
		self.uni.subscriber.settle_control();
		if drained {
			Poll::Ready(())
		} else {
			self.tasks.poll(waiter)
		}
	}
}

/// What serving one incoming uni stream needs from its session.
#[derive(Clone)]
struct Uni {
	subscriber: Subscriber,
	peer_setup: Option<peer::PeerSetup>,
	/// Exactly one SETUP per endpoint.
	seen_setup: std::sync::Arc<std::sync::atomic::AtomicBool>,
	goaway: crate::goaway::Protocol,
	client: bool,
	version: Version,
}

/// Reads one incoming uni stream's type and serves it, reporting to `fatal` only when the
/// stream breaks the session.
struct UniServe {
	uni: Uni,
	step: UniStep,
	fatal: kio::Shared<Option<Error>>,
}

/// Where [`UniServe`] waits.
enum UniStep {
	/// Waiting for the stream's type.
	Peek(Reader<RecvStream, crate::Version>),
	/// Waiting for the peer's whole SETUP.
	Setup(Reader<RecvStream, crate::Version>),
	/// Monitoring the peer's SETUP stream for GOAWAY.
	GoAway(ReadGoaways<RecvStream>),
	Group(Reader<RecvStream, Version>, RecvGroup),
	Fill(Reader<RecvStream, Version>, RecvFill),
	Padding(Reader<RecvStream, crate::Version>),
	/// Between steps.
	Done,
}

impl UniServe {
	fn new(uni: Uni, reader: Reader<RecvStream, crate::Version>, fatal: kio::Shared<Option<Error>>) -> Self {
		let reader = reader.with_stats(uni.subscriber.stats());
		Self {
			uni,
			step: UniStep::Peek(reader),
			fatal,
		}
	}

	/// `Ready` once the stream is served, failing only when it breaks the session.
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let version = self.uni.version;
		loop {
			match &mut self.step {
				UniStep::Peek(reader) => {
					// A stream that dies before its type varint is that stream's failure, not
					// the session's. RESET_STREAM is how a peer drops a group, and QUIC does
					// not order the reset behind the data, so one can beat the first byte even
					// of a stream the peer wrote to. Failing here would tear down the whole
					// session over a single stream the peer had already given up on. Only
					// death is tolerated: bytes that arrive and do not parse stay
					// session-fatal.
					//
					// A transport error counts as death too. A reset whose code the transport
					// cannot place in the stream registry surfaces as one: over raw QUIC a
					// moq-transport peer resets with its own codes (moxygen's CANCELLED is
					// 0x1), which the WebTransport code mapping rejects. If the connection
					// itself died, the next accept reports it.
					let kind: u64 = match ready!(reader.poll_varint_peek(waiter)) {
						Ok(kind) => kind,
						Err(err) if died_before_header(&err) => {
							tracing::debug!(%err, "dropping uni stream that died before its type");
							return Poll::Ready(Ok(()));
						}
						Err(err) => return Poll::Ready(Err(err)),
					};

					let Some(ty) = UniType::classify(kind) else {
						tracing::warn!(kind, "unknown uni stream type");
						return Poll::Ready(Err(Error::UnexpectedStream));
					};

					// Exactly one SETUP per endpoint. A second would let a peer restate its
					// declared identity mid-session, silently re-attributing every route
					// already built from the first.
					if ty == UniType::Setup && self.uni.seen_setup.swap(true, std::sync::atomic::Ordering::Relaxed) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}

					let UniStep::Peek(mut reader) = std::mem::replace(&mut self.step, UniStep::Done) else {
						unreachable!("matched above");
					};
					self.step = match ty {
						// SETUP then becomes the GOAWAY channel. It is read in the background
						// like any other stream; the one thing that does need it (the MoQ
						// Cluster negotiation) waits on `peer_setup` instead, so a slow SETUP
						// delays announcements rather than the whole session.
						//
						// The negotiation gates the announce and dispatch loops, so a SETUP we
						// cannot read must end the session rather than leave them parked on a
						// slot nothing will ever fill.
						UniType::Setup => {
							reader.set_priority(crate::priority::RECV_CONTROL);
							UniStep::Setup(reader)
						}
						UniType::Subgroup => UniStep::Group(reader.with_version(version), RecvGroup::new()),
						// A fill fetch stream carries the head of the group a draft-22
						// subscription joined part way through. One answering no fill of ours
						// is refused inside.
						UniType::Fetch => UniStep::Fill(reader.with_version(version), RecvFill::new()),
						UniType::Padding => UniStep::Padding(reader),
					};
				}
				UniStep::Setup(reader) => {
					let msg = ready!(reader.poll_decode::<setup::Setup>(waiter))?;
					if let Some(peer_setup) = &self.uni.peer_setup {
						let options = decode_peer_setup(&msg.parameters, version, self.uni.client)?;
						peer_setup.set(options.declared);
						peer_setup.set_implementation(options.implementation);
					}

					let UniStep::Setup(reader) = std::mem::replace(&mut self.step, UniStep::Done) else {
						unreachable!("matched above");
					};
					// Monitor for GOAWAY after setup completes.
					self.step = UniStep::GoAway(ReadGoaways::new(
						reader.with_version(version),
						version,
						self.uni.client,
						self.uni.goaway.clone(),
					));
				}
				UniStep::GoAway(goaways) => {
					let err = ready!(goaways.poll(waiter));
					return Poll::Ready(Err(control_stream_error(err)));
				}
				UniStep::Group(reader, recv) => {
					let res = ready!(recv.poll(waiter, &self.uni.subscriber, reader));
					return Poll::Ready(stop_on_error(reader, res));
				}
				UniStep::Fill(reader, recv) => {
					let res = ready!(recv.poll(waiter, &self.uni.subscriber, reader));
					return Poll::Ready(stop_on_error(reader, res));
				}
				// The receiver MUST discard padding. One that dies is not the session's failure.
				UniStep::Padding(reader) => {
					if let Err(err) = ready!(reader.poll_discard(waiter)) {
						tracing::debug!(%err, "padding stream ended");
					}
					return Poll::Ready(Ok(()));
				}
				UniStep::Done => unreachable!("polled after it finished"),
			}
		}
	}
}

impl kio::Task for UniServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			self.fatal.lock().get_or_insert(err);
		}
		Poll::Ready(())
	}
}

/// Whether reading an incoming stream's header failed because the stream died, which is
/// that stream's failure and not the session's. Bytes that arrive and do not parse are not.
fn died_before_header(err: &Error) -> bool {
	matches!(
		err,
		Error::Cancel
			| Error::Stream(_)
			| Error::Remote(_)
			| Error::Transport(_)
			| Error::IdleTimeout
			| Error::Decode(DecodeError::Short)
	)
}

/// Receive QUIC datagrams, each an OBJECT_DATAGRAM for one of our subscriptions.
///
/// A transport failure or a malformed datagram ends the session.
struct Datagrams {
	session: Transport,
	subscriber: Subscriber,
	/// A peer keeping datagrams ready must not hold the session's other tasks.
	budget: kio::coop::Budget,
	/// This pass of the budget is spent, waiting for the next datagram.
	charged: bool,
}

impl Datagrams {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			if !self.charged {
				ready!(self.budget.poll_yield(waiter));
				self.charged = true;
			}
			let payload = ready!(self.session.poll_recv_datagram(waiter)).map_err(Error::from_transport)?;
			self.charged = false;
			self.subscriber.recv_datagram(payload)?;
		}
	}
}

/// Stop a refused data stream, propagating a session-scoped error to session teardown.
pub(super) fn stop_on_error<R: crate::transport::poll::RecvStream>(
	reader: &mut Reader<R, Version>,
	res: Result<(), Error>,
) -> Result<(), Error> {
	let Err(err) = res else {
		return Ok(());
	};
	// An overflowing object ID is a protocol violation, unlike an unsupported gap or a
	// malformed object that costs only its stream.
	if matches!(err, Error::Session(_)) {
		return Err(err);
	}

	tracing::debug!(%err, "uni stream error");
	let reset = match StreamError::from(&err) {
		StreamError::Session(_) => StreamError::Internal,
		reset => reset,
	};
	reader.abort(reset);
	Ok(())
}

/// Accept incoming bidi streams, leaving each request to read its own header. Only an
/// error ends it.
struct Dispatch {
	session: Transport,
	version: Version,
	peer_setup: peer::PeerSetup,
	/// What `bidi` is built from once the peer's SETUP arrives.
	start: Option<(Publisher, Subscriber, Option<auth::Serve>)>,
	/// What each request is served with, once the peer's SETUP says how they decode.
	bidi: Option<Bidi>,
	tasks: TaskSet,
	fatal: kio::Shared<Option<Error>>,
	/// MAX_STREAMS credit alone lets the peer keep thousands of streams ready, and the
	/// children are polled only when accepting pends. A yield pends it.
	budget: kio::coop::Budget,
	/// One pass per accepted stream, not per wake.
	charge: bool,
	/// Each AUTH serve task settles `Issued::closed` from whatever the auth handle
	/// holds when it drops, and the first reason wins. So the error that ends the
	/// session has to reach the handle while `tasks` is still alive, or every grant
	/// reports a bare cancel instead. `None` when the peer never negotiated AUTH,
	/// which leaves no serve task to settle.
	handle: Option<crate::auth::Link>,
}

impl Dispatch {
	fn new(
		session: Transport,
		publisher: Publisher,
		subscriber: Subscriber,
		peer_setup: peer::PeerSetup,
		// Answers the peer's Auth requests, on the versions that can negotiate them.
		serve: Option<auth::Serve>,
		version: Version,
	) -> Self {
		Self {
			session,
			version,
			peer_setup,
			start: Some((publisher, subscriber, serve)),
			bidi: None,
			tasks: TaskSet::owned(),
			fatal: Default::default(),
			budget: kio::coop::Budget::new(32),
			charge: true,
			handle: None,
		}
	}

	/// Build what each request is served with, once the peer's SETUP arrives.
	fn poll_setup(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let (_, subscriber, _) = self.start.as_ref().expect("set up once");

		// PUBLISH_NAMESPACE decodes differently once the MoQ Cluster extension is
		// negotiated, so dispatch waits for SETUP, which `Unis` reads independently.
		let peer = ready!(subscriber.poll_peer(waiter));

		// An AUTH without MoQ Auth negotiated, offered by both sides, is an unknown request,
		// which falls through to the protocol violation in `BidiServe`.
		let negotiated = ready!(self.peer_setup.poll_get(waiter)).auth;

		// From the same slot, so this costs nothing extra: it decides whether an unsolicited
		// advertisement is the peer ignoring our own SETUP (MoQ Solicit).
		let declared = ready!(subscriber.poll_solicit(waiter));

		let (publisher, subscriber, serve) = self.start.take().expect("set up once");
		let serve = match negotiated {
			true => serve,
			false => None,
		};
		self.handle = serve.as_ref().map(|serve| serve.handle.clone());
		self.bidi = Some(Bidi {
			publisher,
			subscriber,
			peer,
			declared,
			serve,
			version: self.version,
		});
		Poll::Ready(())
	}

	/// `Ready` only with the error that ends the session.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		if self.bidi.is_none() {
			ready!(self.poll_setup(waiter));
		}
		let bidi = self.bidi.as_ref().expect("set up above");
		loop {
			let accepted = 'accept: {
				let broken = self.fatal.poll(waiter, |fatal| match fatal.is_some() {
					true => Poll::Ready(()),
					false => Poll::Pending,
				});
				if let Poll::Ready(mut fatal) = broken
					&& let Some(err) = fatal.take()
				{
					break 'accept Poll::Ready(Err(err));
				}
				if std::mem::take(&mut self.charge) && self.budget.poll_yield(waiter).is_pending() {
					break 'accept Poll::Pending;
				}
				Stream::poll_accept(&self.session, self.version, waiter)
			};
			match accepted {
				// QUIC may expose an earlier stream before its bytes arrive. Reading the
				// header here could block later streams whose unread data holds flow control.
				Poll::Ready(Ok(stream)) => {
					self.charge = true;
					self.tasks
						.push(BidiServe::new(bidi.clone(), stream, self.fatal.clone()));
				}
				Poll::Ready(Err(err)) => {
					// `tasks` still holds the AUTH serve tasks, so the session's error lands on
					// the handle before they drop. The driver closes it again with the same
					// error, which then does nothing.
					if let Some(handle) = &self.handle {
						handle.close(err.clone());
					}
					return Poll::Ready(err);
				}
				Poll::Pending => {
					// The children never end the loop; a `Ready` here just means they're
					// drained until the next stream.
					let _ = self.tasks.poll(waiter);
					return Poll::Pending;
				}
			}
		}
	}
}

/// What serving one incoming bidi request needs from its session.
#[derive(Clone)]
struct Bidi {
	publisher: Publisher,
	subscriber: Subscriber,
	peer: cluster::Peer,
	declared: Option<bool>,
	serve: Option<auth::Serve>,
	version: Version,
}

/// Reads one request's header and serves it, reporting to `fatal` only when the request
/// breaks the session.
struct BidiServe {
	bidi: Bidi,
	step: BidiStep,
	fatal: kio::Shared<Option<Error>>,
}

/// Where [`BidiServe`] waits.
enum BidiStep {
	/// Reading the request's type, then its body.
	Header {
		stream: Stream<Transport, Version>,
		id: Option<u64>,
	},
	/// Refusing a request past our GOAWAY cutoff.
	Refuse(Reject),
	/// The publisher or subscriber serving it.
	Serve(crate::util::BoxTask),
	/// Answering an Auth request.
	Auth(Box<auth::Answering>),
	/// Between steps.
	Done,
}

impl BidiServe {
	fn new(bidi: Bidi, stream: Stream<Transport, Version>, fatal: kio::Shared<Option<Error>>) -> Self {
		let stream = stream.with_stats(bidi.publisher.stats());
		Self {
			bidi,
			step: BidiStep::Header { stream, id: None },
			fatal,
		}
	}

	/// `Ready` once the request is served, failing only when it breaks the session.
	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.step {
				BidiStep::Header { stream, id } => {
					// Abandoning a request before its header is not a session failure. Bytes
					// that arrive and do not parse, and dispatch errors, remain session-fatal.
					let (id, data) = match ready!(poll_header(stream, id, waiter)) {
						Ok(header) => header,
						Err(err) if died_before_header(&err) => {
							tracing::debug!(%err, "dropping bidi stream that died before its header");
							return Poll::Ready(Ok(()));
						}
						Err(err) => return Poll::Ready(Err(err)),
					};
					let BidiStep::Header { stream, .. } = std::mem::replace(&mut self.step, BidiStep::Done) else {
						unreachable!("matched above");
					};
					self.step = self.dispatch(id, data, stream)?;
				}
				BidiStep::Refuse(reject) => return reject.poll(waiter),
				BidiStep::Serve(task) => {
					ready!(task(waiter));
					return Poll::Ready(Ok(()));
				}
				BidiStep::Auth(answering) => return answering.poll(waiter),
				BidiStep::Done => unreachable!("polled after it finished"),
			}
		}
	}

	/// Hand a request whose header arrived to whatever serves it.
	fn dispatch(
		&mut self,
		id: u64,
		data: ietf::Body,
		mut stream: Stream<Transport, Version>,
	) -> Result<BidiStep, Error> {
		let Bidi {
			publisher,
			subscriber,
			peer,
			declared,
			serve,
			version,
		} = &mut self.bidi;
		let version = *version;
		stream.reader = stream.reader.with_stats(publisher.stats());
		publisher
			.stats()
			.control(super::message::control(id), data.0.len() + 2, true);
		if matches!(
			id,
			ietf::Subscribe::ID
				| ietf::Fetch::ID
				| ietf::SubscribeNamespace::ID
				| ietf::SubscribeTracks::ID
				| ietf::TrackStatus::ID
				| ietf::Publish::ID
				| ietf::PublishNamespace::ID
				| auth::Auth::ID
		) {
			let request_id = RequestId::decode(&mut data.decoder(version), version)?;
			match publisher.control.accept(request_id) {
				Err(Error::GoingAway) => return Ok(BidiStep::Refuse(refuse_going_away(stream, id, version))),
				result => {
					result?;
				}
			}
		}
		Ok(match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace, SubscribeTracks, TrackStatus
			ietf::Subscribe::ID
			| ietf::Fetch::ID
			| ietf::SubscribeNamespace::ID
			| ietf::SubscribeTracks::ID
			| ietf::TrackStatus::ID => BidiStep::Serve(publisher.handle_stream(id, data, stream)?),
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				BidiStep::Serve(subscriber.handle_stream(id, data, stream, *peer, *declared)?)
			}
			auth::Auth::ID if let Some(serve) = serve.take() => {
				let mut data = data.decoder(version);
				let msg = auth::Auth::decode_msg(&mut data, version)?;
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				BidiStep::Auth(Box::new(serve.answer(stream, msg, version)))
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type");
				return Err(Error::UnexpectedStream);
			}
		})
	}
}

impl kio::Task for BidiServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			self.fatal.lock().get_or_insert(err);
		}
		Poll::Ready(())
	}
}

/// A request's type, then its body. `id` holds the type once it arrived.
fn poll_header(
	stream: &mut Stream<Transport, Version>,
	id: &mut Option<u64>,
	waiter: &kio::Waiter,
) -> Poll<Result<(u64, ietf::Body), Error>> {
	let id = match *id {
		Some(id) => id,
		None => *id.insert(ready!(stream.reader.poll_varint(waiter))?),
	};
	// The type arrived, so a body cut short by FIN is malformed, not the stream dying.
	let body = match ready!(stream.reader.poll_decode::<ietf::Body>(waiter)) {
		Err(Error::Decode(DecodeError::Short)) => return Poll::Ready(Err(Error::WrongSize)),
		body => body?,
	};
	Poll::Ready(Ok((id, body)))
}

/// Refuse a request past our GOAWAY cutoff with the answer its kind reads: an AUTH
/// presenter reads only AUTH_OK and AUTH_ERROR.
fn refuse_going_away(stream: Stream<Transport, Version>, id: u64, version: Version) -> Reject {
	if id == auth::Auth::ID {
		return auth::going_away(stream);
	}
	let kind = super::error::request::Kind::Subscribe;
	Reject::new(stream, ietf::RequestError::refuse(&Error::GoingAway, kind, "going away", version))
}

/// A control stream cannot end independently of its session, so its own failure is the
/// peer's violation. The session closing or timing out under it passes through.
pub(super) fn control_stream_error(err: Error) -> Error {
	if is_protocol_violation(&err) || matches!(err, Error::Session(_) | Error::Encode(_) | Error::IdleTimeout) {
		err
	} else {
		Error::ProtocolViolation
	}
}

/// Reads every message after SETUP on the peer's SETUP stream, which may only be GOAWAY.
/// A FIN or reset of this control stream is a protocol violation, so only an error ends it;
/// [`control_stream_error`] says which one the session closes with.
///
/// Reading continues past the first GOAWAY until the peer FINs or the session closes.
/// Dropping the reader would STOP_SENDING the peer's SETUP uni stream, which draft-22
/// section 6.4.1 forbids closing at the transport layer mid-session, so a strict peer would
/// tear the session down as a PROTOCOL_VIOLATION in the middle of the drain we are honoring.
struct ReadGoaways<R: crate::transport::poll::RecvStream> {
	reader: Reader<R, Version>,
	version: Version,
	client: bool,
	goaway: crate::goaway::Protocol,
	/// The type of the message whose body is arriving.
	id: Option<u64>,
}

impl<R: crate::transport::poll::RecvStream> ReadGoaways<R> {
	fn new(reader: Reader<R, Version>, version: Version, client: bool, goaway: crate::goaway::Protocol) -> Self {
		Self {
			reader,
			version,
			client,
			goaway,
			id: None,
		}
	}

	/// `Ready` only with the error that ends it.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		self.poll_read(waiter).map(|Err(err)| err)
	}

	fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Result<std::convert::Infallible, Error>> {
		let version = self.version;
		loop {
			let id = match self.id {
				Some(id) => id,
				None => match ready!(self.reader.poll_varint_maybe(waiter))? {
					Some(id) => *self.id.insert(id),
					None => return Poll::Ready(Err(Error::ProtocolViolation)),
				},
			};
			// The type arrived, so a body cut short by FIN is malformed, not the stream dying.
			let body = match ready!(self.reader.poll_decode::<ietf::Body>(waiter)) {
				Err(Error::Decode(DecodeError::Short)) => return Poll::Ready(Err(Error::WrongSize)),
				body => body?,
			};
			self.id = None;
			if id != ietf::GoAway::ID {
				return Poll::Ready(Err(Error::UnexpectedMessage));
			}

			let msg: ietf::GoAway = body.message(version)?;
			self.reader
				.stats()
				.control(crate::stats::Message::Goaway, body.0.len() + 2, true);
			tracing::info!(message = ?msg, "received GOAWAY");

			// Draft-18 names the first of our own Request IDs it did not process: even for a
			// client, odd for a server. The field is required here, on the control stream.
			match msg.request_id {
				None if version == Version::Draft18 => return Poll::Ready(Err(Error::ProtocolViolation)),
				Some(request_id) if request_id.0 % 2 != u64::from(!self.client) => {
					return Poll::Ready(Err(DecodeError::Session(SessionError::InvalidRequestId).into()));
				}
				_ => {}
			}

			// Nothing may follow a GOAWAY on this stream, so bytes already here are a violation.
			// Caught before the GOAWAY surfaces: the application acting on it can close the
			// session first, without our code.
			if let Poll::Ready(Ok(true)) = self.reader.poll_has_more(&kio::Waiter::noop()) {
				return Poll::Ready(Err(Error::ProtocolViolation));
			}

			let timeout = (msg.timeout > 0).then(|| std::time::Duration::from_millis(msg.timeout));
			// A second GOAWAY is a protocol violation the draft requires we close over.
			self.goaway.record(crate::goaway::Goaway {
				uri: msg.new_session_uri.into_owned(),
				timeout,
			})?;
		}
	}
}

/// Closes the auth handle when the session driver ends without finishing, releasing
/// anything still waiting on a token.
struct AuthClose(crate::auth::Link);

impl Drop for AuthClose {
	fn drop(&mut self) {
		self.0.close(Error::Cancel);
	}
}

/// Close the session when we publish a broadcast our grant never covered, instead of
/// leaving it to wait for a subscription that never comes. See [`crate::auth::Enforce`].
///
/// Judges the publisher's serving origin rather than the raw one: an origin that also
/// consumes holds what the peer sent us, which is not ours to publish back.
struct EnforceGrant {
	auth: crate::auth::Link,
	publisher: Publisher,
	session: Transport,
	state: EnforceGrantState,
}

#[kio::machine(self.state: EnforceGrantState)]
impl EnforceGrant {
	fn new(auth: crate::auth::Link, publisher: Publisher, session: Transport) -> Self {
		Self {
			auth,
			publisher,
			session,
			state: EnforceGrantState::Start,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> std::task::Poll<Result<(), Error>> {
		let origin: origin::Consumer = wait!(Origin => self.publisher.poll_serving_origin(waiter));
		// Checking what we publish is not a local reader asking to discover anything.
		let mut announced: crate::origin::Routes = origin.clone().serving().routes();
		let mut check: crate::auth::Enforce = crate::auth::Enforce::default();
		let path = wait!(Check => check.poll(&self.auth, announced, waiter));
		let Some(path) = path else {
			return std::task::Poll::Ready(Ok(()));
		};
		tracing::error!(broadcast = %crate::path::Rooted(origin.root(), &path), "publishing outside our grant; closing the session");
		let err = Error::Unauthorized;
		self.session.close(
			super::error::to_session_code(&SessionError::from(&err)),
			&crate::auth::unauthorized_reason(&path),
		);
		std::task::Poll::Ready(Err(err))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::ProduceTest;

	/// The peer's IMPLEMENTATION option is read alongside what it declared, and one that
	/// is not UTF-8 refuses the SETUP.
	#[test]
	fn peer_setup_reads_the_implementation() {
		let version = Version::Draft22;
		let encode = |value: Option<&[u8]>| {
			let mut params = ietf::Parameters::default();
			if let Some(value) = value {
				params.set_bytes(ietf::ParameterBytes::Implementation, value.to_vec());
			}
			params.encode_bytes(version).unwrap()
		};
		let options = decode_peer_setup(&encode(Some(b"moq-net-rs/1")), version, true).unwrap();
		assert_eq!(options.implementation.as_deref(), Some("moq-net-rs/1"));
		let options = decode_peer_setup(&encode(None), version, true).unwrap();
		assert_eq!(options.implementation, None);
		assert!(decode_peer_setup(&encode(Some(&[0xff])), version, true).is_err());
	}

	#[kio::test]
	async fn announce_interest_waits_for_credit() {
		for version in [Version::Draft18] {
			let origin = origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _interest = origin.consume().routes();
			let scripted = crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit();
			let log = scripted.log.clone();
			let session = crate::driver::Transport::new(scripted.clone());
			let (driver, _goaway, _) = start(Config {
				runtime: crate::time::sim(),
				stats: Default::default(),
				session,
				limits: Default::default(),
				client: true,
				publish: None,
				subscribe: Some(origin),
				peer_hop: None,
				cost: None,
				version,
				path: None,
				authority: None,
				token: None,
				peer_setup_stream: None,
				peer_declared: Some(peer::Peer {
					solicit: Some(true),
					..Default::default()
				}),
				auth: crate::auth::Link::new(false),
				early_unis: Vec::new(),
				implementation: None,
				peer_implementation: Default::default(),
				extensions: Default::default(),
			})
			.unwrap();
			let mut driver = std::pin::pin!(driver);
			assert!(futures::poll!(driver.as_mut()).is_pending(), "{version}");
			kio::clock::sleep(std::time::Duration::from_secs(20)).await;
			assert!(
				futures::poll!(driver.as_mut()).is_pending(),
				"{version}: waiting for credit ended the session"
			);
			let opened = log.bi_opens();
			assert!(log.closes().is_empty());

			scripted.grant_credit();
			let _ = futures::poll!(driver.as_mut());
			assert!(log.bi_opens() > opened, "{version}: interest opens once credit returns");
		}
	}

	#[kio::test]
	async fn auth_ids_need_the_peers_parity() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for client in [false, true] {
				let first = u64::from(client);
				for requests in [vec![(true, first ^ 1)], vec![(false, first), (true, (first + 2) ^ 1)]] {
					let incoming = requests
						.into_iter()
						.map(|(is_auth, id)| {
							let mut body = Vec::new();
							let mut encoder = crate::coding::Encoder::new(&mut body, version.into());
							let kind = if is_auth {
								auth::Auth {
									request_id: RequestId(id),
									token: Default::default(),
								}
								.encode_msg(&mut encoder, version)
								.unwrap();
								auth::Auth::ID
							} else {
								ietf::TrackStatus {
									authorization_token: None,
									request_id: RequestId(id),
									track_namespace: crate::Path::from_static("test"),
									track_name: "video".into(),
									properties_wanted: true,
								}
								.encode_msg(&mut encoder, version)
								.unwrap();
								ietf::TrackStatus::ID
							};
							let mut script = Vec::new();
							let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
							encoder.varint(kind).unwrap();
							encoder.u16(body.len() as u16);
							script.extend(body);
							script
						})
						.collect();
					let session =
						crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(incoming);
					let log = session.log.clone();
					let (driver, _, _) = start(Config {
						runtime: crate::time::sim(),
						stats: Default::default(),
						session: crate::driver::Transport::new(session.clone()),
						limits: Default::default(),
						client,
						publish: None,
						subscribe: None,
						peer_hop: None,
						cost: None,
						version,
						path: None,
						authority: None,
						token: None,
						peer_setup_stream: None,
						peer_declared: Some(peer::Peer {
							auth: true,
							..Default::default()
						}),
						auth: crate::auth::Link::new(true),
						early_unis: Vec::new(),
						implementation: None,
						peer_implementation: Default::default(),
						extensions: Default::default(),
					})
					.unwrap();
					let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
						.await
						.expect("invalid AUTH ID left the session open")
						.expect_err("invalid request ID must be fatal");
					assert!(
						matches!(err, Error::Session(SessionError::InvalidRequestId)),
						"{version:?} client={client}: {err:?}"
					);
					assert_eq!(log.closes()[0].0, SessionError::InvalidRequestId.to_code());
				}
			}
		}
	}

	#[kio::test]
	async fn malformed_auth_replies_close_the_session() {
		for version in [Version::Draft22] {
			for (id, body) in [
				(99, vec![]),
				(auth::AuthError::ID, vec![0xc0, 0, 0, 1, 0, 0, 0, 0, 0]),
				(auth::AuthError::ID, vec![1]),
				(auth::AuthError::ID, vec![1, 0, 0]),
				(auth::AuthOk::ID, vec![0, 0, 0xc0, 0x20, 0, 0, 0, 0, 0, 0]),
			] {
				let mut script = Vec::new();
				let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
				encoder.varint(id).unwrap();
				encoder.u16(body.len() as u16);
				script.extend(body);
				let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![script]);
				let log = session.log.clone();
				let (driver, _, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					limits: Default::default(),
					client: true,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer {
						auth: true,
						..Default::default()
					}),
					auth: crate::auth::Link::new(true),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();
				let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("the session outlived a malformed AUTH reply")
					.expect_err("malformed AUTH is fatal");
				assert!(is_protocol_violation(&err), "{err:?}");
				assert_eq!(log.closes()[0].0, SessionError::ProtocolViolation.to_code());
			}
		}
	}

	/// A granted token whose Auth request the peer resets is presented again on the same
	/// session, backing off while each attempt is lost before a reply: at 0s, 1s, and 3s.
	#[kio::test]
	async fn a_lost_auth_request_is_presented_again() {
		let version = Version::Draft18;
		let mut grant = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut grant, version.into());
		encoder.varint(auth::AuthOk::ID).unwrap();
		encoder.u16(3);
		grant.extend([0, 0, 0]);
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![grant.clone(), grant]);
		session.close(crate::lite::test_transport::Close::ResetStream(
			crate::ietf::error::to_stream_code(&crate::StreamError::Internal),
		));
		let log = session.log.clone();
		let handle = crate::auth::Handle::new();
		let (driver, _, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			auth: crate::auth::Link::attach(&handle, true, true).unwrap(),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let token = handle.present(bytes::Bytes::from_static(b"token")).unwrap();

		let res = kio::clock::timeout(std::time::Duration::from_secs(5), driver).await;
		assert!(res.is_err(), "the session ended: {res:?}");
		assert_eq!(occurrences(&log, b"token"), 3);
		assert!(
			futures::FutureExt::now_or_never(token.closed()).is_none(),
			"a lost request ended the token"
		);
	}

	/// A peer that negotiated MoQ Auth but resets a token's request before any reply gave it
	/// nothing: the token fails as unsupported rather than being presented again.
	#[kio::test]
	async fn an_auth_request_reset_before_its_reply_is_unsupported() {
		let version = Version::Draft18;
		let mut grant = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut grant, version.into());
		encoder.varint(auth::AuthOk::ID).unwrap();
		encoder.u16(3);
		grant.extend([0, 0, 0]);
		// The setup token is granted; ours is reset before any reply.
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![grant, Vec::new()]);
		session.close(crate::lite::test_transport::Close::ResetStream(
			crate::ietf::error::to_stream_code(&crate::StreamError::Internal),
		));
		let log = session.log.clone();
		let handle = crate::auth::Handle::new();
		let (driver, _, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			auth: crate::auth::Link::attach(&handle, true, true).unwrap(),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let token = handle.present(bytes::Bytes::from_static(b"token")).unwrap();

		let res = kio::clock::timeout(std::time::Duration::from_secs(5), driver).await;
		assert!(res.is_err(), "the session ended: {res:?}");
		assert_eq!(occurrences(&log, b"token"), 1, "presented again");
		let err = futures::FutureExt::now_or_never(token.closed()).expect("the token is still open");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
	}

	#[kio::test]
	async fn malformed_presenter_auth_closes_the_session() {
		for version in [Version::Draft22] {
			for body in [vec![0, 2], vec![0, 0, 0]] {
				let mut script = Vec::new();
				let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
				encoder.varint(auth::Auth::ID).unwrap();
				encoder.u16(2);
				script.extend(body);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![script]);
				let log = session.log.clone();
				let (driver, _, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					limits: Default::default(),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer {
						auth: true,
						..Default::default()
					}),
					auth: crate::auth::Link::new(true),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();
				let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("malformed presenter AUTH was ignored")
					.expect_err("malformed AUTH is fatal");
				assert!(is_protocol_violation(&err), "{err:?}");
				assert_eq!(log.closes()[0].0, SessionError::ProtocolViolation.to_code());
			}
		}
	}

	#[test]
	fn object_overflow_is_session_fatal_but_a_stream_error_is_not() {
		let mut reader = Reader::new(
			crate::transport::boxed::RecvStream::new(crate::lite::test_transport::PendingRecv),
			Version::Draft22,
		);
		let err = stop_on_error(&mut reader, Err(SessionError::ProtocolViolation.into())).unwrap_err();
		assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation);
		assert!(stop_on_error(&mut reader, Err(Error::Unsupported)).is_ok());
		// A malformed property overflows too, but costs only its stream.
		assert!(stop_on_error(&mut reader, Err(Error::Decode(DecodeError::BoundsExceeded))).is_ok());
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// The peer's REQUEST_OK followed by a NAMESPACE in the base form: no cluster
	/// parameters, so no HOP_PATH. Built with the crate's own writer so the framing
	/// can't drift from the encoder under test.
	async fn namespace_without_hop_path(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await
			.unwrap();
		writer.varint(ietf::Namespace::ID).await.unwrap();
		writer
			.encode(&ietf::Namespace {
				suffix: crate::Path::from_static("cam"),
				params: None,
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A session that negotiated the MoQ Cluster extension requires HOP_PATH on every
	/// NAMESPACE, and the draft answers a missing one by closing the session.
	///
	/// Driven through `start` rather than `run_subscribe_namespace` directly: the
	/// stream surfaces the error either way, so only this loop's handling of it decides
	/// between a close and a warning, and a test below the loop would pass regardless.
	#[kio::test]
	async fn a_namespace_without_a_hop_path_closes_the_session() {
		const VERSION: Version = Version::Draft18;

		// A driver that swallows the violation parks forever instead of failing, so
		// bound it: paused time makes the deadline fire the moment nothing else can run.

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		// A client asks only about what is watched.
		let _watch = origin.consume().routes();
		let session = crate::lite::test_transport::ScriptedSession::new(namespace_without_hop_path(VERSION).await);
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			// A peer that declared its Hop ID negotiated the extension, which is what
			// makes the cluster parameters mandatory in both directions.
			peer_declared: Some(peer::Peer {
				cluster: cluster::Peer {
					hop: Some(crate::Hop::new(2).unwrap()),
					cost: None,
				},
				..Default::default()
			}),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");

		let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect("the session ended rather than carrying on")
			.expect_err("a malformed NAMESPACE fails the session");

		assert!(is_protocol_violation(&err), "not treated as the peer's fault: {err}");
		// A session close carries a session code, so the peer is told PROTOCOL_VIOLATION
		// rather than the local table's value for whichever decode error we hit.
		let (code, reason) = log.closes().first().cloned().expect("the session was closed");
		assert_eq!(code, SessionError::ProtocolViolation.to_code());
		assert_eq!(reason, err.to_string());
	}

	/// A subscriber watching everything issues one SUBSCRIBE_NAMESPACE per PERMITTED
	/// PREFIX, and asks for those prefixes rather than the root it mounts replies under.
	/// Driven through `start` so the per-prefix fan-out is exercised, not just one stream
	/// in isolation: a loop that opened a single stream would still satisfy a test that
	/// called `run_subscribe_namespace` itself.
	#[kio::test]
	async fn every_permitted_prefix_gets_its_own_subscribe_namespace() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let scope: crate::Patterns = ["cam", "mic"]
			.into_iter()
			.map(|prefix| crate::Pattern::subtree(prefix).unwrap())
			.collect();
		let scoped = origin
			.scope("rootns", &scope)
			.expect("scope the origin to two prefixes");
		let _watch = scoped.consume().routes();

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(scoped),
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			// The requests wait on the peer's SETUP (MoQ Hidden).
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let _driver = kio::clock::sim::spawn(driver);

		// Both requests are written before either peer response, which never comes.
		for _ in 0..100 {
			if occurrences(&log, b"cam") > 0 && occurrences(&log, b"mic") > 0 {
				break;
			}
			kio::clock::sleep(std::time::Duration::from_millis(5)).await;
		}

		assert_eq!(occurrences(&log, b"cam"), 1, "one SUBSCRIBE_NAMESPACE for cam");
		assert_eq!(occurrences(&log, b"mic"), 1, "one SUBSCRIBE_NAMESPACE for mic");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// The bytes of one SUBSCRIBE_NAMESPACE, as the session writes them.
	async fn subscribe_namespace(version: Version, namespace: &str, request_id: RequestId) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::SubscribeNamespace {
			authorization_token: None,
			request_id,
			namespace: crate::Path::new(namespace).unwrap(),
			hidden: false,
		};
		writer.varint(ietf::SubscribeNamespace::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Watch everything under `prefix`.
	fn watch(origin: &crate::origin::Producer, prefix: &str) -> crate::origin::Routes {
		origin
			.consume()
			.scope(prefix, &crate::Patterns::from(crate::Pattern::all()))
			.unwrap()
			.routes()
	}

	/// Start a client feeding `subscribe` against a peer that answers nothing and declared
	/// `solicit` (MoQ Solicit).
	async fn client(
		version: Version,
		subscribe: crate::origin::Producer,
		solicit: Option<bool>,
	) -> (
		crate::lite::test_transport::Log,
		kio::clock::sim::JoinHandle<Result<(), Error>>,
	) {
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		client_on(version, session, subscribe, solicit).await
	}

	/// [`client`] over `session`.
	async fn client_on(
		version: Version,
		session: crate::lite::test_transport::SinkSession,
		subscribe: crate::origin::Producer,
		solicit: Option<bool>,
	) -> (
		crate::lite::test_transport::Log,
		kio::clock::sim::JoinHandle<Result<(), Error>>,
	) {
		let log = session.log.clone();
		let session = crate::driver::Transport::new(session);

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session,
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(subscribe),
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				solicit,
				..Default::default()
			}),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		(log, kio::clock::sim::spawn(driver))
	}

	/// Let the driver run until `needle` reaches the wire, or give up after a while.
	async fn sent(log: &crate::lite::test_transport::Log, needle: &[u8]) -> bool {
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, needle) > 0 {
				return true;
			}
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}
		false
	}

	/// A client asks nothing until something is watched, then only about what is.
	#[kio::test]
	async fn a_client_asks_only_about_what_is_watched() {
		for version in [Version::Draft18] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let (log, _driver) = client(version, subscribe.clone(), Some(true)).await;
			let first = subscribe_namespace(version, "cam", RequestId(0)).await;
			assert!(
				!sent(&log, &first).await,
				"{version:?} asked before anything was watched"
			);

			let _cam = watch(&subscribe, "cam");
			assert!(sent(&log, &first).await, "{version:?} did not ask about the watch");
			let second = subscribe_namespace(version, "", RequestId(2)).await;
			assert!(
				!sent(&log, &second).await,
				"{version:?} asked about more than the watch"
			);
		}
	}

	/// Each request has a stream of its own, so at a foreign peer the broader one waits for
	/// the peer to close the narrower one's, which ends with a FIN on draft-18 and a
	/// reset on draft-22: a peer that never closes it never sees the two overlap.
	#[kio::test]
	async fn a_broader_watch_waits_for_the_narrower_to_leave_the_wire() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _front = watch(&subscribe, "cam/front");
			let (log, _driver) = client(version, subscribe.clone(), None).await;
			assert!(sent(&log, b"front").await, "{version:?}");
			let opened = log.bi_opens();

			let _cam = watch(&subscribe, "cam");
			for _ in 0..ANNOUNCE_TURNS {
				kio::clock::sleep(std::time::Duration::from_millis(1)).await;
			}
			assert_eq!(log.bi_opens(), opened, "{version:?} overlapped the narrower request");
			let ended = log.trail().into_iter().any(|(_, sent)| match sent {
				crate::lite::test_transport::Sent::Finish => super::super::request_stream::fin_cancels(version),
				crate::lite::test_transport::Sent::Reset(_) => !super::super::request_stream::fin_cancels(version),
				crate::lite::test_transport::Sent::Write(_) => false,
			});
			assert!(ended, "{version:?} did not end the narrower request: {:?}", log.trail());
		}
	}

	/// A request ended while it waits on the peer for stream credit holds nothing: the
	/// broader request that replaced it goes out alone once the credit arrives.
	#[kio::test]
	async fn a_request_ended_before_it_opened_does_not_block_the_broader_one() {
		for version in [Version::Draft22] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let gate = kio::Producer::new(true);
			let credit = kio::Producer::new(true);
			let session =
				crate::lite::test_transport::SinkSession::gated_bi(gate.consume()).with_open_bi_gate(credit.consume());
			let (log, _driver) = client_on(version, session, subscribe.clone(), None).await;
			*credit.write().ok().unwrap() = false;
			let opened = log.bi_opens();

			let _front = watch(&subscribe, "cam/front");
			for _ in 0..ANNOUNCE_TURNS {
				kio::clock::sleep(std::time::Duration::from_millis(1)).await;
			}
			let _cam = watch(&subscribe, "cam");
			for _ in 0..ANNOUNCE_TURNS {
				kio::clock::sleep(std::time::Duration::from_millis(1)).await;
			}
			*credit.write().ok().unwrap() = true;
			assert!(sent(&log, b"cam").await, "{version:?} never asked the broader prefix");
			assert_eq!(log.bi_opens(), opened + 1, "{version:?} opened the replaced request");
			assert!(!sent(&log, b"front").await, "{version:?} sent the replaced request");
		}
	}

	/// A peer that declared MoQ Solicit is ours and holds overlapping requests, so a broader
	/// watch leaves the narrower request open, as on moq-lite: neither takes the other's
	/// routes over before it has synced.
	#[kio::test]
	async fn a_broader_watch_keeps_the_narrower_open_at_our_peer() {
		for version in [Version::Draft18] {
			let subscribe = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _front = watch(&subscribe, "cam/front");
			let (log, _driver) = client(version, subscribe.clone(), Some(true)).await;
			let asked = subscribe_namespace(version, "cam/front", RequestId(0)).await;
			assert!(sent(&log, &asked).await, "{version:?}");

			let _cam = watch(&subscribe, "cam");
			let broader = subscribe_namespace(version, "cam", RequestId(2)).await;
			assert!(sent(&log, &broader).await, "{version:?} never asked the broader prefix");
			let ended = log
				.trail()
				.iter()
				.any(|(_, sent)| !matches!(sent, crate::lite::test_transport::Sent::Write(_)));
			assert!(!ended, "{version:?} ended the narrower request: {:?}", log.trail());
		}
	}

	/// How many scheduling turns an advertisement gets before the count is taken. Time is
	/// paused in these tests, so each turn costs nothing and only runs the driver until it
	/// parks again; a busy machine cannot turn a slow announce into a passing silence.
	const ANNOUNCE_TURNS: usize = 100;

	/// Run a publish-only session against a peer that declared `peer_declared`, returning
	/// how many times the namespace reached the wire.
	///
	/// The scripted peer answers nothing, so an advertisement parks after writing. That is
	/// enough: the question is only whether the bytes went out unasked.
	async fn announce_occurrences(peer_declared: Option<peer::Peer>) -> usize {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin
			.announce("solo-cam", crate::origin::RouteInfo::default())
			.unwrap();

		// An open gate: an unsolicited PUBLISH_NAMESPACE can reach the wire.
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared,
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		let _driver = kio::clock::sim::spawn(driver);

		// Drive until the announce lands, rather than betting on one fixed window.
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}

		occurrences(&log, b"solo-cam")
	}

	/// The peer's SETUP decides whether an advertisement may go out unasked, so nothing
	/// can be sent before it arrives.
	#[kio::test]
	async fn no_announce_before_the_peer_setup() {
		assert_eq!(
			announce_occurrences(None).await,
			0,
			"advertised before knowing what the peer wants"
		);
	}

	/// A peer that requires solicitation hears nothing until it asks, which is the
	/// behavior the IETF draft describes for a relay.
	#[kio::test]
	async fn a_peer_requiring_solicitation_is_not_told_unasked() {
		let declared = peer::Peer {
			solicit: Some(true),
			..Default::default()
		};

		assert_eq!(
			announce_occurrences(Some(declared)).await,
			0,
			"PUBLISH_NAMESPACE despite the peer asking to be told on request"
		);
	}

	/// A peer that declared nothing is told without being asked. Every relay that never
	/// sends SUBSCRIBE_NAMESPACE depends on this, and the session is what wires the
	/// unsolicited loop up at all.
	#[kio::test]
	async fn a_peer_declaring_nothing_is_told_unasked() {
		assert_eq!(
			announce_occurrences(Some(peer::Peer::default())).await,
			1,
			"no unsolicited PUBLISH_NAMESPACE"
		);
	}

	/// The AUTH message type as it leads an Auth request on the draft-18 wire.
	fn auth_type() -> Vec<u8> {
		let mut buf = Vec::new();
		crate::coding::Encoder::new(&mut buf, Version::Draft18.into())
			.varint(auth::Auth::ID)
			.unwrap();
		buf
	}

	/// A publishing draft-18 session with AUTH available locally, against a peer that
	/// declared `auth`. Everything the session needs to keep running is held here.
	struct AuthSession {
		handle: crate::auth::Link,
		log: crate::lite::test_transport::Log,
		_origin: crate::origin::Producer,
		_cam: crate::AnnounceProducer,
		_gate: kio::Producer<bool>,
		_goaway: crate::goaway::Handle,
		_driver: kio::clock::sim::JoinHandle<Result<(), Error>>,
	}

	fn auth_session(auth: bool) -> AuthSession {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let cam = origin
			.announce("solo-cam", crate::origin::RouteInfo::default())
			.unwrap();

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let handle = crate::auth::Link::ietf(true);
		let (driver, goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth,
				..Default::default()
			}),
			auth: handle.clone(),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");
		AuthSession {
			handle,
			log,
			_origin: origin,
			_cam: cam,
			_gate: gate,
			_goaway: goaway,
			_driver: kio::clock::sim::spawn(driver),
		}
	}

	/// A peer that never offered MoQ Auth sees no Auth request and the session keeps
	/// working. A token resolves promptly rather than hanging, kept with an unknown grant
	/// for the requests that name it.
	#[kio::test]
	async fn a_peer_without_auth_sees_no_auth_request() {
		let session = auth_session(false);
		let (handle, log) = (&session.handle, &session.log);

		let (auth, answer) = kio::clock::timeout(std::time::Duration::from_secs(1), handle.handle().add("token"))
			.await
			.expect("resolved promptly")
			.expect("kept for requests");
		assert_eq!(answer, crate::auth::Answer::Requests);
		assert_eq!(auth.grant().peek(), None, "no grant without the extension");
		assert_eq!(handle.handle().grant().peek(), None, "no grant without the extension");

		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, b"solo-cam") > 0 {
				break;
			}
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}
		assert_eq!(occurrences(log, b"solo-cam"), 1, "the session stopped advertising");
		assert_eq!(
			occurrences(log, &auth_type()),
			0,
			"sent AUTH to a peer that never offered it"
		);
	}

	/// A peer that offered it gets the connection's own credential right away.
	#[kio::test]
	async fn a_negotiating_peer_gets_the_setup_token() {
		let session = auth_session(true);
		let log = &session.log;
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(log, &auth_type()) > 0 {
				break;
			}
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}
		assert_eq!(occurrences(log, &auth_type()), 1, "one AUTH for the setup token");
	}

	/// What the draft-18 SETUP that `start` writes says about MoQ Auth and MoQ Solicit.
	async fn our_setup(client: bool, auth: crate::auth::Link, extensions: crate::setup::Extensions) -> (bool, bool) {
		const VERSION: Version = Version::Draft18;

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth,
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions,
		})
		.expect("start the session");

		// The driver parks once SETUP is out.
		kio::clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended instead of parking after SETUP");

		let writes = log.writes.lock().unwrap().clone();
		let (setup, _) = setup::Setup::decode_slice(&writes, crate::Version::Ietf(VERSION)).expect("our SETUP");
		let (params, _) = ietf::Parameters::decode_slice(&setup.parameters, VERSION).expect("its options");
		(
			auth::from_setup(&params) == Some(true),
			solicit::from_setup(&params, VERSION).unwrap() == Some(true),
		)
	}

	/// Each extension reaches our SETUP only while this side offers it.
	#[kio::test]
	async fn a_declined_extension_stays_out_of_setup() {
		let all = crate::setup::Extensions::default();
		let mut no_auth = all;
		no_auth.auth = false;
		let mut no_solicit = all;
		no_solicit.solicit = false;

		// A client's handle supports AUTH exactly when it offers it.
		assert_eq!(our_setup(true, crate::auth::Link::new(true), all).await, (true, true));
		assert_eq!(
			our_setup(true, crate::auth::Link::new(false), no_auth).await,
			(false, true)
		);
		assert_eq!(
			our_setup(true, crate::auth::Link::new(true), no_solicit).await,
			(true, false)
		);
	}

	/// A server's handle supports AUTH only when the client offered it too, so a server
	/// answering a client that did not offer it leaves the option out, whatever it offers.
	#[kio::test]
	async fn a_server_offers_auth_only_to_a_client_that_did() {
		let all = crate::setup::Extensions::default();
		assert_eq!(our_setup(false, crate::auth::Link::new(true), all).await, (true, true));
		assert_eq!(
			our_setup(false, crate::auth::Link::new(false), all).await,
			(false, true)
		);
	}

	/// The bytes of a peer's Auth request presenting its connection credential.
	async fn auth_request(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		writer
			.encode_message(&auth::Auth {
				request_id: RequestId(0),
				token: bytes::Bytes::new(),
			})
			.await
			.unwrap();
		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Run a draft-18 server whose client offered MoQ Auth and then sent an Auth request.
	/// `offered` is whether the server offered it too. Returns how the session ended, if it
	/// did, and what closed it.
	async fn serve_auth_request(offered: bool) -> (Option<Result<(), Error>>, Vec<(u32, String)>) {
		const VERSION: Version = Version::Draft18;

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
			.with_incoming_bidis(vec![auth_request(VERSION).await]);
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: false,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer {
				auth: true,
				..Default::default()
			}),
			// As `Server` builds it: offered by the client, so supported iff we offer it.
			auth: crate::auth::Link::new(offered),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: crate::setup::Extensions::default(),
		})
		.expect("start the session");

		let res = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.ok();
		(res, log.closes())
	}

	/// The Auth draft negotiates the extension only when both sides offer it, so an Auth
	/// request reaching a side that declined it is a protocol violation, not a token to grant.
	#[kio::test]
	async fn an_auth_request_to_a_side_that_declined_it_is_refused() {
		let (res, closes) = serve_auth_request(true).await;
		assert!(res.is_none(), "both offered: the request is served, {res:?}");
		assert_eq!(closes, vec![]);

		let (res, closes) = serve_auth_request(false).await;
		let Some(Err(err)) = res else {
			panic!("declined: the Auth request did not end the session: {res:?}");
		};
		assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation);
		assert_eq!(
			closes,
			vec![(SessionError::ProtocolViolation.to_code(), err.to_string())]
		);
	}

	/// The declared Hop ID must be the caller's own origin, whichever half carries it.
	///
	/// A subscribe-only session (an ingest that publishes nothing) still routes, so
	/// declaring the placeholder's random id would compare incoming paths against an
	/// identity no other session out of this process shares, and a route returning
	/// here would never be recognized as a loop.
	#[test]
	fn the_hop_id_comes_from_whichever_origin_the_caller_set() {
		let ours = crate::Hop::new(42).unwrap();

		let publish = crate::origin::Config::new(ours).produce();
		assert_eq!(self_origin(Some(&publish.consume()), None), ours, "the publish half");

		let subscribe = crate::origin::Config::new(ours).produce();
		assert_eq!(self_origin(None, Some(&subscribe)), ours, "the subscribe half alone");

		// Neither half: nothing to route, so any id will do as long as it is ours.
		let publish = crate::origin::Config::new(ours).produce();
		assert_eq!(self_origin(Some(&publish.consume()), Some(&subscribe)), ours);
	}

	/// Drive `start` against a peer whose incoming streams die before their first
	/// byte, and assert the session shrugs them off: the driver keeps running and
	/// nothing closes the transport.
	///
	/// The dead stream is not exotic. RESET_STREAM is how a peer drops a group, and
	/// QUIC does not order the reset behind the data, so it can beat the first byte
	/// even when the peer wrote one. Erroring an accept loop on it tears down the
	/// whole session over a single stream the peer had already given up on.
	async fn a_dead_incoming_stream_is_not_fatal(session: crate::lite::test_transport::DeadStreamSession) {
		const VERSION: Version = Version::Draft18;

		// A driver that survives parks forever, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			// Pre-settled, so nothing waits on a SETUP the dead stream will never
			// carry and the dispatch loop actually runs.
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");

		kio::clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended over one dead stream");

		assert_eq!(log.closes(), vec![], "nothing may close the transport");
	}

	/// A client advertises its AUTHORITY and connection credential in the SETUP it writes on
	/// its uni stream.
	#[kio::test]
	async fn setup_carries_the_authority_and_token() {
		const AUTHORITY: &[u8] = b"relay.example.com:4443";
		const TOKEN: &[u8] = b"connection-credential";

		// The driver parks forever once SETUP is out, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.

		let version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: Some(String::from_utf8(AUTHORITY.to_vec()).unwrap()),
			token: Some(crate::setup::Token {
				kind: crate::setup::Token::OUT_OF_BAND,
				value: TOKEN.to_vec(),
			}),
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.expect("start the session");

		kio::clock::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended instead of parking after SETUP");

		assert_eq!(
			occurrences(&log, AUTHORITY),
			1,
			"{version:?}: SETUP must carry the authority once"
		);
		assert_eq!(
			occurrences(&log, TOKEN),
			1,
			"{version:?}: SETUP must carry the token once"
		);
	}

	#[kio::test]
	async fn a_uni_stream_dead_before_its_type_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1)).await;
	}

	#[kio::test]
	async fn a_bidi_stream_dead_before_its_header_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1)).await;
	}

	/// moxygen resets a subgroup stream it opened but never wrote with its own CANCELLED
	/// (0x1). Over raw QUIC our transport reads that code through the WebTransport space
	/// and cannot map it, which used to end the session.
	#[kio::test]
	async fn a_uni_stream_reset_with_an_unmapped_code_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1).unmapped()).await;
	}

	#[kio::test]
	async fn a_bidi_stream_reset_with_an_unmapped_code_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1).unmapped()).await;
	}

	/// The bytes a publisher writes at the head of a group's unidirectional stream. Built
	/// with the crate's own encoder so the framing can't drift from the decoder the
	/// dispatch loop runs.
	async fn subgroup_header(version: Version, track_alias: u64, sub_group_id: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

		writer
			.encode(&ietf::GroupHeader {
				track_alias,
				group_id: 0,
				sub_group_id,
				publisher_priority: 128,
				flags: ietf::GroupFlags {
					has_subgroup: sub_group_id != 0,
					..Default::default()
				},
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Run the uni dispatch loop over one incoming stream. Returns what reached the wire,
	/// and the loop's result if that stream ended it.
	async fn dispatch_uni(
		version: Version,
		payload: Vec<u8>,
		retired_alias: Option<u64>,
	) -> (crate::lite::test_transport::Log, Option<Result<(), Error>>) {
		// The peer opens one uni stream and then goes quiet, so
		// the loop is still running when the assertion is taken.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![payload]);
		drive_unis(version, session, None, retired_alias).await
	}

	/// Run the uni dispatch loop until it stops a stream or ends. `accepted` is a SETUP
	/// already read by [`accept_setup`], with the streams that raced ahead of it.
	async fn drive_unis(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
		accepted: Option<PeerSetup>,
		retired_alias: Option<u64>,
	) -> (crate::lite::test_transport::Log, Option<Result<(), Error>>) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let log = session.log.clone();

		let (tasks, _task_set) = TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(true),
			None,
			peer_setup.clone(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		if let Some(alias) = retired_alias {
			subscriber.retire_alias(alias);
		}

		// Held so the trigger side stays alive for as long as the loop runs.
		let (_goaway, goaway) = crate::goaway::Handle::new(false);
		// Without `accepted`, the peer's SETUP has not arrived yet, which is the state a
		// group stream racing ahead of it lands in.
		let read = accepted.is_some();
		let early = accepted.map(|accepted| accepted.early).unwrap_or_default();
		let mut unis = Unis::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			UniSetup {
				peer: Some(peer_setup),
				read,
				early,
				client: true,
				version,
				goaway,
			},
		);

		for _ in 0..100 {
			if let std::task::Poll::Ready(result) = unis.poll(&kio::Waiter::noop()) {
				return (log, Some(result));
			}
			if !log.stops().is_empty() {
				break;
			}
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}

		(log, None)
	}

	/// A peer opening uni streams at once gets a budget of them accepted per poll, leaving
	/// the rest for later so the session's other tasks run in between.
	#[kio::test]
	async fn a_flood_of_uni_streams_yields() {
		const VERSION: Version = Version::Draft18;
		let (local, peer) = crate::transport::memory::pair(Default::default());
		let (tasks, _task_set) = TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(local.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(true),
			None,
			peer_setup.clone(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);
		let (_goaway, goaway) = crate::goaway::Handle::new(false);
		let mut unis = Unis::new(
			crate::driver::Transport::new(local.clone()),
			subscriber,
			UniSetup {
				peer: Some(peer_setup),
				read: false,
				early: Vec::new(),
				client: true,
				version: VERSION,
				goaway,
			},
		);
		// Held open and empty, so each accepted stream waits on its type.
		let mut opened = Vec::new();
		for _ in 0..1024 {
			opened.push(peer.open_uni(Default::default()).await.unwrap());
		}

		assert!(unis.poll(&kio::Waiter::noop()).is_pending());
		assert!(
			local.poll_accept_uni(&kio::Waiter::noop()).is_ready(),
			"one poll accepted every stream"
		);
	}

	/// A peer opening request streams at once gets a budget of them accepted per poll,
	/// leaving the rest for later so the session's other tasks run in between.
	#[kio::test]
	async fn a_flood_of_request_streams_yields() {
		const VERSION: Version = Version::Draft18;
		let (local, peer) = crate::transport::memory::pair(Default::default());
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let (tasks, _task_set) = TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let transport = crate::driver::Transport::new(local.clone());
		let publisher = Publisher::new(
			crate::time::sim(),
			transport.clone(),
			origin.consume(),
			Control::new(true),
			None,
			peer_setup.clone(),
			VERSION,
		);
		let subscriber = Subscriber::new(
			crate::time::sim(),
			transport.clone(),
			origin,
			Control::new(true),
			None,
			peer_setup.clone(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);
		let mut dispatch = Dispatch::new(transport, publisher, subscriber, peer_setup, None, VERSION);
		// Held open and empty, so each accepted stream waits on its header.
		let mut opened = Vec::new();
		for _ in 0..1024 {
			opened.push(peer.open_bi(Default::default()).await.unwrap());
		}

		assert!(dispatch.poll(&kio::Waiter::noop()).is_pending());
		assert!(
			local.poll_accept_bi(&kio::Waiter::noop()).is_ready(),
			"one poll accepted every stream"
		);
	}

	/// A late group must reach the dispatch loop and stop with CANCELLED.
	#[kio::test]
	async fn a_group_for_a_retired_alias_is_stopped_with_cancelled() {
		let (log, result) =
			dispatch_uni(Version::Draft18, subgroup_header(Version::Draft18, 7, 0).await, Some(7)).await;

		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"the group stream must be stopped with the cancelled code",
		);
		assert!(result.is_none(), "one dropped group ended the session: {result:?}");
		assert_eq!(log.closes(), vec![], "one dropped group may not close the session");
	}

	/// A stream whose type has not arrived must not hold up the streams accepted after it.
	///
	/// QUIC opens every lower-numbered stream when a higher one arrives, so the next stream
	/// accepted can be one whose bytes the peer has not sent yet, held back by connection
	/// flow control that the later streams' unread data is using up. Waiting on its type
	/// before reading them deadlocks the connection.
	#[kio::test]
	async fn a_silent_stream_does_not_hold_up_the_next() {
		let header = subgroup_header(Version::Draft18, 7, 0).await;
		let session =
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![Vec::new(), header]);
		let (log, result) = drive_unis(Version::Draft18, session, None, Some(7)).await;

		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"the stream behind the silent one was never read",
		);
		assert!(result.is_none(), "the silent stream ended the session: {result:?}");
	}

	/// A stalled bidi header must not block a SUBSCRIBE on the next stream.
	#[kio::test]
	async fn a_silent_bidi_does_not_hold_up_the_next() {
		const VERSION: Version = Version::Draft18;
		let mut subscribe = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut subscribe, VERSION.into());
		encoder.varint(ietf::Subscribe::ID).unwrap();
		ietf::Subscribe {
			max_age: None,
			new_group: None,
			request_id: RequestId(1),
			track_namespace: crate::Path::from_static("room"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: ietf::GroupOrder::Descending,
			filter: ietf::Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
		}
		.encode(&mut encoder, VERSION)
		.unwrap();

		// Stall before the type, then after it while the body has not arrived.
		for stalled in [Vec::new(), vec![ietf::Subscribe::ID as u8]] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let broadcast = origin.route("room", crate::origin::RouteOptions::default()).unwrap();
			let mut track = broadcast.broadcast().create_track("video", None).unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
				.with_incoming_bidis(vec![stalled, subscribe.clone()]);
			let driver = bidi_driver(session, Some(origin.consume()));
			let subscribed = Box::pin(async { while track.subscription_changed().await.unwrap().is_none() {} });
			let result = kio::clock::timeout(
				std::time::Duration::from_secs(10),
				futures::future::select(driver, subscribed),
			)
			.await
			.expect("SUBSCRIBE behind the stalled header was never dispatched");
			assert!(
				matches!(result, futures::future::Either::Right(_)),
				"session ended before SUBSCRIBE"
			);
		}
	}

	/// Child tasks must report malformed and unknown requests to the session driver.
	#[kio::test]
	async fn invalid_bidi_requests_still_end_the_session() {
		for id in [ietf::Subscribe::ID, 0x3f] {
			let mut header = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut header, Version::Draft18.into());
			encoder.varint(id).unwrap();
			encoder.slice(&[0, 0]); // Complete, empty body: malformed for SUBSCRIBE.
			let session =
				crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![header]);
			let err = kio::clock::timeout(std::time::Duration::from_secs(10), bidi_driver(session, None))
				.await
				.expect("invalid request did not end the session")
				.expect_err("invalid request was accepted");
			if id == ietf::Subscribe::ID {
				assert!(matches!(err, Error::Decode(_)), "{err:?}");
			} else {
				assert!(matches!(err, Error::UnexpectedStream), "{err:?}");
			}
		}
	}

	/// Start a draft-22 session with SETUP already settled, so bidi requests run immediately.
	fn bidi_driver(session: crate::lite::test_transport::ScriptedSession, publish: Option<origin::Consumer>) -> Driver {
		start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap()
		.0
	}

	/// A non-zero subgroup is refused on its own stream, never by closing the session.
	#[kio::test]
	async fn a_non_zero_subgroup_is_stopped_without_closing_the_session() {
		let (log, result) = dispatch_uni(Version::Draft18, subgroup_header(Version::Draft18, 7, 1).await, None).await;

		assert_eq!(log.stops(), vec![crate::ietf::error::INTERNAL_ERROR]);
		assert!(result.is_none(), "one refused subgroup ended the session: {result:?}");
		assert_eq!(log.closes(), vec![], "one refused subgroup may not close the session");
	}

	/// A stream type encoded for `version`, followed by a few bytes of body.
	fn uni_stream(version: Version, kind: u64) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut buf, version.into());
		w.varint(kind).unwrap();
		w.slice(&[0; 4]);
		buf
	}

	/// Padding is read and dropped: no STOP_SENDING, and the session stays up.
	#[kio::test]
	async fn a_padding_stream_is_discarded() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let (log, result) = dispatch_uni(version, uni_stream(version, PADDING), None).await;

			assert!(
				log.stops().is_empty(),
				"{version:?}: padding was stopped: {:?}",
				log.stops()
			);
			assert!(result.is_none(), "{version:?}: padding ended the session: {result:?}");
			assert_eq!(log.closes(), vec![], "{version:?}");
		}
	}

	/// A server waiting on the client's SETUP holds the padding and group streams that beat
	/// it, then classifies them once the session starts. The drafts say to buffer early
	/// data; rejecting it reached the wire as INTERNAL_ERROR.
	#[kio::test]
	async fn uni_streams_before_setup_are_held_until_it_lands() {
		const VERSION: Version = Version::Draft18;

		let mut setup = Vec::new();
		setup::Setup {
			parameters: ietf::Parameters::default().encode_bytes(VERSION).unwrap(),
		}
		.encode(
			&mut crate::coding::Encoder::new(&mut setup, VERSION.into()),
			crate::Version::Ietf(VERSION),
		)
		.unwrap();

		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![
			uni_stream(VERSION, PADDING),
			subgroup_header(VERSION, 7, 0).await,
			setup,
		]);
		let accepted = accept_setup(&crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.expect("accept the SETUP");
		assert_eq!(accepted.early.len(), 2, "both early streams are held");
		assert!(session.log.stops().is_empty(), "stopped {:?}", session.log.stops());

		// The group answers a retired subscription, so the classifier reaching it shows
		// as CANCELLED; the padding is read to its end and stops nothing.
		let (log, result) = drive_unis(VERSION, session, Some(accepted), Some(7)).await;
		assert_eq!(log.stops(), vec![crate::ietf::error::CANCELLED]);
		assert!(result.is_none(), "an early stream ended the session: {result:?}");
		assert_eq!(log.closes(), vec![]);
	}

	/// An unknown or invalid stream type MUST close the session, so it stops nothing on its own:
	/// the session close takes the stream with it.
	#[kio::test]
	async fn an_unknown_uni_type_closes_the_session() {
		for (version, kind) in [
			(Version::Draft18, 0),
			// SUBGROUP_HEADER types with the reserved SUBGROUP_ID_MODE (0b11).
			(Version::Draft18, 0x56),
		] {
			let (log, result) = dispatch_uni(version, uni_stream(version, kind), None).await;

			let Some(Err(err)) = result else {
				panic!("{version:?}: type {kind:#x} did not end the session: {result:?}");
			};
			assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation, "{version:?}");
			assert!(log.stops().is_empty(), "{version:?}: stopped {:?}", log.stops());
		}
	}

	/// The bidi dispatcher closes the session for an unknown full-width message type.
	#[kio::test]
	async fn an_unknown_bidi_type_closes_the_session() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for kind in [0u64, 1 << 53] {
				let mut payload = Vec::new();
				let mut w = crate::coding::Encoder::new(&mut payload, version.into());
				w.varint(kind).unwrap();
				w.u16(0);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					limits: Default::default(),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Link::new(false),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();

				let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("unknown bidi type must end the session")
					.expect_err("unknown bidi type must fail the session");
				assert_eq!(
					SessionError::from(&err),
					SessionError::ProtocolViolation,
					"{version:?}: {kind:#x}"
				);
				assert_eq!(
					log.closes(),
					vec![(SessionError::ProtocolViolation.to_code(), err.to_string())]
				);
			}
		}
	}

	#[kio::test]
	async fn an_invalid_group_order_closes_the_session() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for (value, nested) in [
				(0u8, false),
				(3, false),
				(255, false),
				(0, true),
				(3, true),
				(255, true),
			] {
				// FILL_PARAMETERS starts in draft-22.
				if nested && version == Version::Draft18 {
					continue;
				}
				let mut body = vec![0, 1, 1, b'a', 1, b'b', 1];
				if nested {
					body.extend([0x23, 3, 1, 0x22, value]);
				} else {
					body.extend([0x22, value]);
				}
				let mut payload = Vec::new();
				let mut w = crate::coding::Encoder::new(&mut payload, version.into());
				w.varint(ietf::Subscribe::ID).unwrap();
				w.u16(body.len() as u16);
				w.slice(&body);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session: crate::driver::Transport::new(session.clone()),
					client: false,
					publish: None,
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Link::new(false),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
					limits: Default::default(),
				})
				.unwrap();
				let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.expect("invalid GROUP_ORDER must close the session")
					.expect_err("invalid GROUP_ORDER must fail");
				assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation);
				assert_eq!(
					log.closes(),
					vec![(SessionError::ProtocolViolation.to_code(), err.to_string())]
				);
			}
		}
	}

	/// A SUBSCRIBE for `room/video` whose parameters are one AUTHORIZATION TOKEN holding
	/// `token`, framed as a control message.
	fn subscribe_with_token(version: Version, token: &[u8]) -> Vec<u8> {
		let mut body = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut body, version.into());
		// The request ID, namespace and name; every parameter but the token defaults.
		w.slice(&[0, 1, 4, b'r', b'o', b'o', b'm', 5, b'v', b'i', b'd', b'e', b'o']);
		let mut w = crate::coding::Encoder::new(&mut body, version.into());
		w.varint(1).unwrap();
		w.varint(u64::from(ietf::ParameterBytes::AuthorizationToken)).unwrap();
		w.bytes(token).unwrap();

		let mut payload = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut payload, version.into());
		w.varint(ietf::Subscribe::ID).unwrap();
		w.u16(body.len() as u16);
		w.slice(&body);
		payload
	}

	/// A request's AUTHORIZATION TOKEN decodes by the SETUP option's rules, so one this
	/// endpoint cannot use closes the session with the draft's code.
	#[kio::test]
	async fn a_request_token_that_cannot_be_used_closes_the_session() {
		for version in [Version::Draft22] {
			for (token, expected, why) in [
				(
					&[0x01, 0x07, 0x00][..],
					SessionError::AuthTokenCacheOverflow,
					"REGISTER",
				),
				(&[0x00, 0x07][..], SessionError::UnknownAuthTokenAlias, "DELETE"),
				(&[0x02, 0x07][..], SessionError::UnknownAuthTokenAlias, "USE_ALIAS"),
				(
					&[0x03][..],
					SessionError::KeyValueFormatting,
					"USE_VALUE without a type",
				),
				(
					&[0x04, 0x00][..],
					SessionError::KeyValueFormatting,
					"an unknown alias type",
				),
			] {
				let payload = subscribe_with_token(version, token);
				let session =
					crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(vec![payload]);
				let log = session.log.clone();
				let session = crate::driver::Transport::new(session.clone());
				let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
				let (driver, _goaway, _) = start(Config {
					runtime: crate::time::sim(),
					stats: Default::default(),
					session,
					limits: Default::default(),
					client: false,
					publish: Some(origin.consume()),
					subscribe: None,
					peer_hop: None,
					cost: None,
					version,
					path: None,
					authority: None,
					token: None,
					peer_setup_stream: None,
					peer_declared: Some(peer::Peer::default()),
					auth: crate::auth::Link::new(false),
					early_unis: Vec::new(),
					implementation: None,
					peer_implementation: Default::default(),
					extensions: Default::default(),
				})
				.unwrap();

				let err = kio::clock::timeout(std::time::Duration::from_secs(10), driver)
					.await
					.unwrap_or_else(|_| panic!("{version:?} {why}: the session stayed up"))
					.expect_err("the token must fail the session");
				assert_eq!(SessionError::from(&err), expected, "{version:?} {why}");
				assert_eq!(
					log.closes(),
					vec![(expected.to_code(), err.to_string())],
					"{version:?} {why}"
				);
			}
		}
	}

	/// A framed message: its type, a `u16` length, then `body`.
	fn frame(version: Version, id: u64, body: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = crate::coding::Encoder::new(&mut buf, version.into());
		w.varint(id).unwrap();
		w.u16(body.len() as u16);
		w.slice(body);
		buf
	}

	/// The peer's SETUP stream: a SETUP with `params`, then `after`.
	fn setup_stream(version: Version, params: ietf::Parameters, after: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		setup::Setup {
			parameters: params.encode_bytes(version).unwrap(),
		}
		.encode(
			&mut crate::coding::Encoder::new(&mut buf, version.into()),
			crate::Version::Ietf(version),
		)
		.unwrap();
		buf.extend_from_slice(after);
		buf
	}

	/// A GOAWAY with an empty New Session URI, and on draft-18 the control stream's Request ID.
	fn goaway(version: Version, request_id: Option<u64>) -> Vec<u8> {
		let msg = ietf::GoAway {
			new_session_uri: "".into(),
			timeout: 0,
			request_id: request_id.map(RequestId),
		};
		let mut body = Vec::new();
		msg.encode_msg(&mut crate::coding::Encoder::new(&mut body, version.into()), version)
			.unwrap();
		frame(version, ietf::GoAway::ID, &body)
	}

	/// The session code a client closes with after reading `stream` as the server's SETUP
	/// stream, or `None` while the session stays up.
	async fn client_reads_setup_stream(version: Version, stream: Vec<u8>, fin: bool) -> Option<SessionError> {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![stream]);
		if fin {
			session.close(crate::lite::test_transport::Close::Fin);
		}
		let (_, result) = drive_unis(version, session, None, None).await;
		result.map(|result| SessionError::from(&result.expect_err("the loop ended without an error")))
	}

	/// What a server may not send in its SETUP closes the session with the draft's code.
	#[kio::test]
	async fn a_server_setup_with_path_or_authority_closes_the_session() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for (param, expected) in [
				(ietf::ParameterBytes::Path, SessionError::InvalidPath),
				(ietf::ParameterBytes::Authority, SessionError::InvalidAuthority),
			] {
				let mut params = ietf::Parameters::default();
				params.set_bytes(param, b"a".to_vec());
				let closed = client_reads_setup_stream(version, setup_stream(version, params, &[]), false).await;
				assert_eq!(closed, Some(expected), "{version:?}");
			}
		}
	}

	/// The server's AUTHORIZATION TOKEN is decoded even though a client has no use for it: a
	/// malformed one closes the session, and a registration overflowing our cache of 0 is
	/// read as the value it carries (draft-22 section 9.1.4).
	#[kio::test]
	async fn a_server_setup_token_decodes_by_the_setup_rules() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for (token, expected) in [
				// An Alias Type of 4 does not exist.
				(&[0x04][..], Some(SessionError::KeyValueFormatting)),
				// REGISTER alias 7, type 0, empty value.
				(&[0x01, 0x07, 0x00][..], None),
			] {
				let mut params = ietf::Parameters::default();
				params.set_bytes(ietf::ParameterBytes::AuthorizationToken, token.to_vec());
				let closed = client_reads_setup_stream(version, setup_stream(version, params, &[]), false).await;
				assert_eq!(closed, expected, "{version:?} {token:?}");
			}
		}
	}

	/// The SETUP stream carries one GOAWAY and nothing else: anything more, or a GOAWAY that
	/// is malformed, closes the session as a PROTOCOL_VIOLATION. One GOAWAY leaves it up.
	#[kio::test]
	async fn the_setup_stream_carries_at_most_one_goaway() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let request_id = (version == Version::Draft18).then_some(0);
			let one = goaway(version, request_id);
			let two = [one.clone(), one.clone()].concat();
			let unknown = frame(version, 0x3f, &[]);
			let after_goaway = [one.clone(), unknown.clone()].concat();
			let mut oversized = Vec::new();
			let mut w = crate::coding::Encoder::new(&mut oversized, version.into());
			w.string(&"x".repeat(8193)).unwrap();
			w.varint(0u64).unwrap();
			let oversized = frame(version, ietf::GoAway::ID, &oversized);
			// The length covers the URI but not the timeout after it.
			let short = frame(version, ietf::GoAway::ID, &[0]);
			let mut truncated = one.clone();
			truncated.pop();

			for (after, expected, why) in [
				(&one, None, "one GOAWAY"),
				(&two, Some(SessionError::ProtocolViolation), "two GOAWAYs"),
				(&unknown, Some(SessionError::ProtocolViolation), "an unknown message"),
				(
					&after_goaway,
					Some(SessionError::ProtocolViolation),
					"an unknown message after GOAWAY",
				),
				(&oversized, Some(SessionError::ProtocolViolation), "an 8193 byte URI"),
				(
					&short,
					Some(SessionError::ProtocolViolation),
					"a body short of its fields",
				),
				(
					&truncated,
					Some(SessionError::ProtocolViolation),
					"a body cut short by FIN",
				),
			] {
				let stream = setup_stream(version, ietf::Parameters::default(), after);
				let closed = client_reads_setup_stream(version, stream, why == "a body cut short by FIN").await;
				assert_eq!(closed, expected, "{version:?}: {why}");
			}
		}
	}

	#[kio::test]
	async fn a_control_stream_fin_or_reset_is_a_protocol_violation() {
		use crate::lite::test_transport::{Close, DeadStreamSession, ScriptedSession};
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let session = ScriptedSession::new(Vec::new()).with_incoming_unis(vec![setup_stream(
				version,
				ietf::Parameters::default(),
				&[],
			)]);
			session.close(Close::Fin);
			let (_, result) = drive_unis(version, session, None, None).await;
			assert!(
				matches!(result, Some(Err(ref err)) if SessionError::from(err) == SessionError::ProtocolViolation),
				"{version:?}: {result:?}"
			);

			let session = DeadStreamSession::unis(1);
			let recv = session.accept_uni().await.unwrap();
			let (_handle, protocol) = crate::goaway::Handle::new(false);
			let mut goaways = ReadGoaways::new(
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
				version,
				true,
				protocol,
			);
			let err = control_stream_error(kio::wait(|waiter| goaways.poll(waiter)).await);
			assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation, "{version:?}");
		}
	}

	/// A second GOAWAY arriving with the first closes the session before the first surfaces,
	/// so an application leaving over it cannot close first, without our code.
	#[kio::test]
	async fn a_goaway_with_more_behind_it_never_surfaces() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let request_id = (version == Version::Draft18).then_some(0);
			let two = [goaway(version, request_id), goaway(version, request_id)].concat();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_unis(vec![two]);
			let stream = kio::wait(|waiter| session.poll_accept_uni(waiter)).await.unwrap();
			let (handle, protocol) = crate::goaway::Handle::new(false);

			let mut goaways = ReadGoaways::new(
				Reader::new(crate::transport::boxed::RecvStream::new(stream), version),
				version,
				true,
				protocol,
			);
			// A second GOAWAY is fatal.
			let err = kio::wait(|waiter| goaways.poll(waiter)).await;
			assert_eq!(SessionError::from(&err), SessionError::ProtocolViolation, "{version:?}");
			assert!(
				!matches!(
					handle.consumer().poll(&kio::Waiter::noop()),
					std::task::Poll::Ready(Ok(_))
				),
				"{version:?}: the first GOAWAY surfaced"
			);
		}
	}

	/// A fatal error closes the session before its streams drop. A reset of our SETUP stream
	/// reaching a strict peer first has it close over that, and it never sees our code.
	#[kio::test]
	async fn the_session_closes_before_its_streams_reset() {
		const VERSION: Version = Version::Draft22;
		let mut params = ietf::Parameters::default();
		params.set_bytes(ietf::ParameterBytes::Path, b"a".to_vec());
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new())
			.with_incoming_unis(vec![setup_stream(VERSION, params, &[])]);
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client: true,
			publish: None,
			subscribe: None,
			peer_hop: None,
			cost: None,
			version: VERSION,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: None,
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let _ = kio::clock::timeout(std::time::Duration::from_secs(10), driver).await;

		let closes = log.closes();
		assert_eq!(closes.len(), 1, "{closes:?}");
		assert_eq!(SessionError::from_code(closes[0].0), SessionError::InvalidPath);
		let before = log.before_close().unwrap();
		assert!(
			!before
				.trail
				.iter()
				.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Reset(_))),
			"reset before the close: {before:?}"
		);
		assert!(before.stops.is_empty(), "STOP_SENDING before the close: {before:?}");
	}

	/// A draft-18 GOAWAY names one of the receiver's own Request IDs, so one with the sender's
	/// parity closes the session with INVALID_REQUEST_ID rather than redirecting it, and one
	/// without a Request ID closes it as a PROTOCOL_VIOLATION.
	#[kio::test]
	async fn a_draft18_goaway_request_id_has_the_receivers_parity() {
		const VERSION: Version = Version::Draft18;
		// The loop runs as a client, whose Request IDs are even.
		for (request_id, expected) in [
			(Some(0), None),
			(Some(4), None),
			(Some(1), Some(SessionError::InvalidRequestId)),
			(Some(5), Some(SessionError::InvalidRequestId)),
			// Required on the control stream.
			(None, Some(SessionError::ProtocolViolation)),
		] {
			let stream = setup_stream(VERSION, ietf::Parameters::default(), &goaway(VERSION, request_id));
			let closed = client_reads_setup_stream(VERSION, stream, false).await;
			assert_eq!(closed, expected, "Request ID {request_id:?}");
		}
	}

	/// Draft-18 GOAWAY names the first peer ID past every one used, preserving the peer's
	/// parity, and still admits one a reordered stream skipped below it.
	#[kio::test]
	async fn our_draft18_goaway_names_the_admitted_request_cutoff() {
		const VERSION: Version = Version::Draft18;
		for client in [false, true] {
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
			let log = session.log.clone();
			let control = Control::new(client);
			let first = u64::from(client);
			control.accept(RequestId(first)).unwrap();
			control.accept(RequestId(first + 4)).unwrap();
			let (handle, goaway_protocol) = crate::goaway::Handle::new(!client);
			let mut setup = SendSetup::new(
				crate::driver::Transport::new(session.clone()),
				VERSION,
				SetupParams {
					path: None,
					authority: None,
					token: None,
					implementation: None,
					extensions: Default::default(),
				},
				Hop::new(1).unwrap(),
				None,
				goaway_protocol,
				control.clone(),
				Default::default(),
			);
			handle.producer().send(crate::goaway::Goaway::new()).unwrap();
			assert!(setup.poll(&kio::Waiter::noop()).is_pending());
			let sent = goaway(VERSION, Some(first + 6));
			assert_eq!(occurrences(&log, &sent), 1, "client={client}");
			control.accept(RequestId(first + 2)).unwrap();
			assert!(matches!(control.accept(RequestId(first + 6)), Err(Error::GoingAway)));
		}
	}

	/// Drive a session that reads `payloads` as bidi streams the peer opened, and return the
	/// code it closed with, or `None` while it stays up.
	///
	/// Read off the transport, since a request task closes the session itself and the
	/// scripted transport does not end the driver for it.
	async fn serve_bidis(version: Version, client: bool, payloads: Vec<Vec<u8>>) -> Option<SessionError> {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_incoming_bidis(payloads);
		// The peer FINs each stream after its script, so a message it cuts short is malformed.
		session.close(crate::lite::test_transport::Close::Fin);
		let log = session.log.clone();
		let (driver, _goaway, _) = start(Config {
			runtime: crate::time::sim(),
			stats: Default::default(),
			session: crate::driver::Transport::new(session.clone()),
			limits: Default::default(),
			client,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_hop: None,
			cost: None,
			version,
			path: None,
			authority: None,
			token: None,
			peer_setup_stream: None,
			peer_declared: Some(peer::Peer::default()),
			auth: crate::auth::Link::new(false),
			early_unis: Vec::new(),
			implementation: None,
			peer_implementation: Default::default(),
			extensions: Default::default(),
		})
		.unwrap();
		let _ = kio::clock::timeout(std::time::Duration::from_secs(10), driver).await;
		let closes = log.closes();
		// A request stream closes at the source and its task may close again; the transport
		// keeps the first, so only a disagreeing code would reach the peer.
		assert!(
			closes.iter().all(|(code, _)| *code == closes[0].0),
			"closed with different codes: {closes:?}"
		);
		closes.first().map(|(code, _)| SessionError::from_code(*code))
	}

	/// A request past our GOAWAY cutoff is refused in the answer its kind reads: an AUTH
	/// with AUTH_ERROR carrying GOING_AWAY, since its presenter takes anything else as a
	/// violation, and the rest with REQUEST_ERROR.
	#[kio::test]
	async fn a_request_past_our_goaway_is_refused_in_its_own_answer() {
		const VERSION: Version = Version::Draft18;
		let reason = b"going away";
		let body = [&[0x6, reason.len() as u8][..], reason].concat();
		for (id, answer) in [
			(auth::Auth::ID, frame(VERSION, auth::AuthError::ID, &body)),
			(
				ietf::Subscribe::ID,
				frame(VERSION, ietf::RequestError::ID, &[&[0x6, 0][..], &body[1..]].concat()),
			),
		] {
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
			let log = session.log.clone();
			let stream = Stream::open(&crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let mut refusal = refuse_going_away(stream, id, VERSION);
			kio::wait(|waiter| refusal.poll(waiter)).await.unwrap();
			assert_eq!(*log.writes.lock().unwrap(), answer, "{id:#x}");
		}
	}

	/// A malformed request closes the session with the draft's code: a Request ID of the
	/// wrong parity, a namespace or name outside its bounds, or a body that does not fill
	/// its length.
	#[kio::test]
	async fn a_malformed_request_closes_the_session() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let subscribe = |body: &[u8]| frame(version, ietf::Subscribe::ID, body);
			// Request ID 1, the namespace "n", the name "x", and no parameters.
			let valid = [1, 1, 1, b'n', 1, b'x', 0];

			let mut oversized_name = vec![1, 1];
			let mut w = crate::coding::Encoder::new(&mut oversized_name, version.into());
			w.bytes(&[b'n'; 4096]).unwrap();
			w.bytes(b"x").unwrap();
			w.varint(0u64).unwrap();

			let mut oversized_prefix = vec![1, 1];
			let mut w = crate::coding::Encoder::new(&mut oversized_prefix, version.into());
			w.bytes(&[b'n'; 4097]).unwrap();
			w.varint(0u64).unwrap();

			let mut truncated = subscribe(&valid);
			truncated.pop();

			for (payload, expected, why) in [
				(
					frame(version, ietf::SubscribeNamespace::ID, &[0, 0, 0]),
					SessionError::InvalidRequestId,
					"a Request ID with our parity",
				),
				(
					subscribe(&[1, 1, 0, 1, b'x', 0]),
					SessionError::ProtocolViolation,
					"an empty namespace field",
				),
				(
					subscribe(&oversized_name),
					SessionError::ProtocolViolation,
					"a 4097 byte full track name",
				),
				(
					frame(version, ietf::SubscribeTracks::ID, &oversized_prefix),
					SessionError::ProtocolViolation,
					"a 4097 byte SUBSCRIBE_TRACKS prefix",
				),
				(
					subscribe(&[&valid[..], &[0]].concat()),
					SessionError::ProtocolViolation,
					"a body longer than its fields",
				),
				(truncated, SessionError::ProtocolViolation, "a body cut short by FIN"),
			] {
				let closed = serve_bidis(version, true, vec![payload]).await;
				assert_eq!(closed, Some(expected), "{version:?}: {why}");
			}
		}
	}

	/// A SUBSCRIBE_NAMESPACE for every namespace, then `after` on the same stream.
	fn subscribe_namespace_then(version: Version, request_id: u64, after: &[u8]) -> Vec<u8> {
		let mut body = Vec::new();
		ietf::SubscribeNamespace {
			authorization_token: None,
			request_id: RequestId(request_id),
			namespace: crate::Path::from_static(""),
			hidden: false,
		}
		.encode_msg(&mut crate::coding::Encoder::new(&mut body, version.into()), version)
		.unwrap();
		[frame(version, ietf::SubscribeNamespace::ID, &body), after.to_vec()].concat()
	}

	/// A request stream may carry one GOAWAY, migrating that request alone: it leaves the
	/// session up, even on two streams at once. A second on one stream, or an unknown
	/// message, closes the session as a PROTOCOL_VIOLATION.
	#[kio::test]
	async fn a_request_stream_carries_at_most_one_goaway() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let one = goaway(version, None);
			let two = [one.clone(), one.clone()].concat();

			let distinct = vec![
				subscribe_namespace_then(version, 1, &one),
				subscribe_namespace_then(version, 3, &one),
			];
			assert!(
				serve_bidis(version, true, distinct).await.is_none(),
				"{version:?}: a GOAWAY on each of two streams"
			);

			let mut malformed = vec![(two, "two GOAWAYs"), (frame(version, 0x7e, &[]), "an unknown message")];
			// Draft-18's Request ID belongs on the control stream only.
			if version == Version::Draft18 {
				malformed.push((goaway(version, Some(0)), "a GOAWAY with a Request ID"));
			}
			for (after, why) in malformed {
				let closed = serve_bidis(version, true, vec![subscribe_namespace_then(version, 1, &after)]).await;
				assert_eq!(closed, Some(SessionError::ProtocolViolation), "{version:?}: {why}");
			}
		}
	}

	/// Only a server may name a new session in a request stream GOAWAY: a client that does
	/// closes the session as a PROTOCOL_VIOLATION.
	#[kio::test]
	async fn a_request_stream_goaway_redirects_only_from_a_server() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut body = Vec::new();
			ietf::GoAway {
				new_session_uri: "https://example.com/moq".into(),
				timeout: 0,
				request_id: None,
			}
			.encode_msg(&mut crate::coding::Encoder::new(&mut body, version.into()), version)
			.unwrap();
			let redirect = frame(version, ietf::GoAway::ID, &body);

			// A server's Request IDs are odd and a client's even.
			for (client, request_id, expected) in [(true, 1, None), (false, 0, Some(SessionError::ProtocolViolation))] {
				let closed = serve_bidis(
					version,
					client,
					vec![subscribe_namespace_then(version, request_id, &redirect)],
				)
				.await;
				assert_eq!(closed, expected, "{version:?}: served as client={client}");
			}
		}
	}
}
