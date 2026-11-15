//! Point-to-point sessions over an origin, the way TCP connects two hosts.
//!
//! An [`Endpoint`] is a server, a client, or both. As a server it announces the hidden
//! broadcast `<prefix>/.server/<id>` at its first accept, and peers dial it there. As a
//! client it announces `<prefix>/.client/<id>` at its first dial, and dials servers. The
//! broadcasts are used only to route sessions, so ordinary grants decide each endpoint's
//! roles. Each connection is a session: two tracks, one in the dialer's client broadcast
//! and one in the acceptor's server broadcast, both named
//! `track/<dialer>/<acceptor>/<session>`. Sessions are never announced or listed.
//!
//! Each half of a session is a whole track, so messages are groups, with the track's own
//! priority, delay budget and order. Dial with [`Endpoint::dial_track`], accept with
//! `accept_track` or [`Endpoint::poll_accept_track`], and read and write a [`TrackStream`].
//! The leading `track` names the protocol; a dial in any other is reset.
//!
//! A dialer `A` picks a random session id `S` and subscribes to `<prefix>/.server/<B>` for
//! the track `track/A/B/S`. Endpoint `B` surfaces that request as an [`Incoming`] dial
//! from `A`. To accept it, `B` subscribes to `<prefix>/.client/<A>`
//! for the same track name, and `A` answers only a session it dialed, so the exchange
//! checks that `A` really asked `B`: a dial forged in `A`'s name is reset. Naming both ends
//! binds a session to them, so an endpoint `A` dialed cannot replay the session to a third
//! endpoint `C`: `C` resets a name for another acceptor, and `A` never dialed
//! `track/A/C/S`. Each side then writes its own half and reads the peer's.
//!
//! See `doc/concept/socket.md` for the convention other languages follow.

use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, ready};

use crate::{Error, Path, SessionError, StreamError};

use super::WeakEntry;
use super::origin::{self, RouteOptions};
use super::track::{self, Delay, Order, Subscription};

/// Refuse an id that is not one plain path segment: empty, or holding a `/` or a `\`.
fn check_id(id: &str) -> Result<(), Error> {
	if id.is_empty() || id.contains(['/', '\\']) {
		return Err(crate::InvalidPattern::InvalidSegment(id.to_string()).into());
	}
	Ok(())
}

/// What an endpoint's broadcast is for: a server accepts dials, and a client dials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
	Server,
	Client,
}

impl Role {
	/// The segment the role's broadcasts sit under.
	fn segment(self) -> &'static str {
		match self {
			Self::Server => ".server",
			Self::Client => ".client",
		}
	}
}

/// `<prefix>/<role>/<id>`, the endpoint `id`'s broadcast in `role`.
fn endpoint_path(prefix: &Path, role: Role, id: &str) -> Result<Path, Error> {
	check_id(id)?;
	Ok(prefix.join(&Path::from_static(role.segment()))?.join(&Path::new(id)?)?)
}

/// Whether `path` is an endpoint's broadcast, which only routes sessions and carries no
/// media, so a worker that acts on every announced broadcast can skip it.
pub fn is_endpoint(path: &Path) -> bool {
	let parent = path.as_str().rsplit('/').nth(1);
	[Role::Server, Role::Client]
		.into_iter()
		.any(|role| parent == Some(role.segment()))
}

/// A fresh session id: 128 random bits as hex, so nobody can guess a session's tracks. Drawn
/// from the ambient generator, the thread's outside a simulated run, so a simulated run
/// replays them.
fn session_id() -> Arc<str> {
	use rand::RngExt;
	format!("{:032x}", kio::clock::rng().random::<u128>()).into()
}

/// The track name of a session: `<protocol>/<dialer>/<acceptor>/<session>`, the same in
/// both endpoints.
fn track_name(protocol: &str, dialer: &str, acceptor: &str, session: &str) -> String {
	format!("{protocol}/{dialer}/{acceptor}/{session}")
}

/// A session's track name, split.
#[derive(Debug, PartialEq, Eq)]
struct Name<'a> {
	protocol: &'a str,
	dialer: &'a str,
	acceptor: &'a str,
	session: &'a str,
}

/// Split a session's track name, or `None` for a name outside the scheme.
fn parse(name: &str) -> Option<Name<'_>> {
	let mut segments = name.split('/');
	let [protocol, dialer, acceptor, session] = std::array::from_fn(|_| segments.next().unwrap_or_default());
	if segments.next().is_some() {
		return None;
	}
	for segment in [protocol, dialer, acceptor, session] {
		check_id(segment).ok()?;
	}
	Some(Name {
		protocol,
		dialer,
		acceptor,
		session,
	})
}

/// A `track` session half's default: every message stays until read, rather than only
/// the newest as for an authored track.
fn messages() -> track::Info {
	track::Info::default().with_max_age(None)
}

/// The protocol segment leading every session's track name.
const PROTOCOL: &str = "track";

/// What a session's reader asks for: every message from group 0, oldest first, however
/// late it is, so a burst written before the answer is kept.
fn subscription() -> Subscription {
	Subscription::default()
		.with_order(Order::Ascending)
		.with_delay(Delay::default().with_max(None))
		.with_range(0..)
}

/// What an endpoint's handles share.
struct Shared {
	id: Arc<str>,
	prefix: Path,
	/// Where this endpoint's broadcasts are announced, at first use.
	publisher: origin::Producer,
	/// Where the peers' endpoints are requested.
	origin: origin::Consumer,
	/// The sessions withholding the broadcasts from their peer.
	withheld: origin::HiddenWithheld,
	state: kio::Lock<State>,
}

/// A `track` session: each half is a whole track, and each message a group.
///
/// [`Self::consumer`] starts subscribed reliably and in order: oldest first, with no
/// delay budget, so a late group is waited for rather than skipped. Each hop's cache
/// still reclaims a group nobody read within its idle window (30 s by default), or under
/// memory pressure, except the newest, so a reader that stalls longer can miss groups.
/// Any group the track reports it will never deliver, such as one a moq-transport
/// publisher refused to fetch, is skipped the same way rather than ending the session.
/// Update its subscription, such as with a finite
/// [`Delay`] or [`Order::Descending`], to make it lossy or newest first, and read it in
/// group order with [`track::Subscriber::ordered`]. Groups are written with
/// [`track::Producer::append_group`], each with its own priority on the wire.
///
/// Finishing [`Self::producer`] closes the session for the peer, and aborting it is an
/// error. The endpoint closing aborts it too. When the peer's half ends with an error,
/// abort this side's half as well, and drop the stream, so both halves end.
pub struct TrackStream {
	/// This side's half: the session's track in this endpoint's broadcast.
	pub producer: track::Producer,
	/// The peer's half: the session's track in the peer's broadcast, subscribed reliably
	/// and in order until updated.
	pub consumer: track::Subscriber,
	peer: Arc<str>,
	session: Arc<str>,
}

impl TrackStream {
	/// The peer endpoint's id.
	pub fn peer(&self) -> &str {
		&self.peer
	}

	/// The session's id, which the dialer chose.
	pub fn session(&self) -> &str {
		&self.session
	}
}

/// An endpoint's handler for the requests to its server broadcast: the dials peers send.
///
/// A poller takes it out to poll it with no lock held, since taking a request wakes the
/// dialer's subscription, and an inline executor can re-enter the endpoint from that wake.
/// The client broadcast has none, so it answers a request for a session it does not hold
/// with [`Error::NotFound`] at once.
enum Handler {
	/// Not made yet, until the first accept makes the server broadcast with it, or no
	/// longer, once the endpoint ended.
	None,
	/// Waiting for a poller.
	Idle(super::broadcast::Requests),
	/// Taken by a poller, which puts it back and wakes the pollers that found it taken,
	/// so each polls it, and is woken by it, in turn.
	Taken(kio::WaiterList),
}

/// What the endpoint's pollers share.
struct State {
	/// The route of the broadcast this endpoint dials from, from its first dial on.
	client: Option<origin::Route>,
	/// The route of the broadcast peers dial, from its first accept on.
	server: Option<origin::Route>,
	/// Where the dials peers send arrive.
	handler: Handler,
	/// Dials from peers, waiting to be accepted.
	dials: VecDeque<track::Request>,
	/// The accepts waiting for a dial.
	waiting: kio::WaiterList,
	/// This side's half of every session handed out, aborted when the endpoint closes.
	streams: Vec<track::TrackWeak>,
	/// Why the endpoint stopped, once it did.
	end: Option<Error>,
}

impl State {
	/// Keep `producer`'s track, so the endpoint closing ends it.
	fn hold(&mut self, producer: &track::Producer) {
		self.streams.retain(|stream| !stream.is_closed());
		self.streams.push(producer.weak());
	}
}

impl Shared {
	/// This endpoint's broadcast in `role`, hidden until announced.
	fn create(&self, role: Role) -> Result<origin::Route, Error> {
		self.publisher
			.route(endpoint_path(&self.prefix, role, &self.id)?, RouteOptions::hidden())
	}

	/// The route of the broadcast this endpoint dials from, announced by its first dial.
	///
	/// Made outside the lock, since announcing wakes local consumers, and announced only
	/// once it is the endpoint's, so a racing dial's spare is never seen.
	fn client(&self) -> Result<origin::Route, Error> {
		{
			let state = self.state.lock();
			if let Some(err) = &state.end {
				return Err(err.clone());
			}
			if let Some(route) = &state.client {
				return Ok(route.clone());
			}
		}
		let route = self.create(Role::Client)?;
		let mut state = self.state.lock();
		if let Some(err) = &state.end {
			return Err(err.clone());
		}
		if let Some(route) = &state.client {
			return Ok(route.clone());
		}
		state.client = Some(route.clone());
		drop(state);
		route.announce()?;
		Ok(route)
	}

	/// Make the broadcast peers dial, with its handler, and announce it: the first accept.
	///
	/// Called by the poller holding [`Handler::Taken`], so no other makes one.
	fn serve(&self) -> Result<super::broadcast::Requests, Error> {
		let route = self.create(Role::Server)?;
		// Before announcing, so the first dial finds it.
		let requests = route.broadcast().requests();
		let mut state = self.state.lock();
		if let Some(err) = &state.end {
			return Err(err.clone());
		}
		state.server = Some(route.clone());
		drop(state);
		route.announce()?;
		Ok(requests)
	}

	/// Queue every dial on hand and take the next, registering `waiter` for more, and fail
	/// with the endpoint's end once it stopped. The first call announces the
	/// server broadcast, and fails as [`origin::Producer::route`] does when the origin may
	/// not publish it.
	///
	/// A request on the server broadcast naming this endpoint as the dialer names no dial
	/// to it, since its own dials live in its client broadcast: it is reset with
	/// [`Error::NotFound`]. So is a request naming another acceptor, such as a session
	/// replayed from the endpoint it was dialed to. A request in another protocol than
	/// `track` is reset with [`Error::Unsupported`].
	///
	/// The handler is polled with no lock held (see [`Handler`]), and resets and wakes go
	/// out after the lock is released.
	fn poll_dial(&self, waiter: &kio::Waiter) -> Poll<Result<track::Request, Error>> {
		let taken = {
			let mut state = self.state.lock();
			if state.end.is_some() {
				None
			} else if let Handler::Taken(waiting) = &mut state.handler {
				waiting.register(waiter);
				None
			} else {
				match std::mem::replace(&mut state.handler, Handler::Taken(kio::WaiterList::new())) {
					Handler::Idle(requests) => Some(Some(requests)),
					// No server broadcast yet: this poller makes it.
					Handler::None => Some(None),
					Handler::Taken(_) => unreachable!("checked above"),
				}
			}
		};
		let mut requests = match taken {
			Some(None) => match self.serve() {
				Ok(requests) => Some(requests),
				Err(err) => {
					// Given back for the next accept to try, waking those that found it taken,
					// unless the endpoint closed meanwhile and took it.
					let mut state = self.state.lock();
					let waiting = match &mut state.handler {
						Handler::Taken(waiting) => Some(waiting.take()),
						_ => None,
					};
					if waiting.is_some() {
						state.handler = Handler::None;
					}
					drop(state);
					if let Some(mut waiting) = waiting {
						waiting.wake();
					}
					return Poll::Ready(Err(err));
				}
			},
			taken => taken.flatten(),
		};
		let mut taken = Vec::new();
		let mut ended = None;
		if let Some(requests) = &mut requests {
			loop {
				match requests.poll_requested_track(waiter) {
					Poll::Pending => break,
					Poll::Ready(Ok(request)) => taken.push(request),
					Poll::Ready(Err(err)) => {
						ended = Some(err);
						break;
					}
				}
			}
		}

		let mut resets = Vec::new();
		let mut wakes = Vec::new();
		let mut retired = None;
		let mut state = self.state.lock();
		if let Some(err) = ended {
			state.end.get_or_insert(err);
		}
		// Put the handler back, unless the endpoint ended meanwhile: then it is dropped
		// after the lock is released, with the server broadcast if `close` hasn't taken it.
		if let Some(handler) = requests.take() {
			let next = match state.end {
				None => Handler::Idle(handler),
				Some(_) => {
					retired = Some((handler, state.server.take()));
					Handler::None
				}
			};
			if let Handler::Taken(mut waiting) = std::mem::replace(&mut state.handler, next) {
				wakes.push(waiting.take());
			}
		}
		// Dials whose dialer gave up leave the queue before an accept takes one, so it holds
		// no more than the dials still open, and an accept gets a live one.
		if state.dials.iter().any(|request| !request.demand().is_wanted()) {
			let (open, abandoned): (VecDeque<_>, VecDeque<_>) = std::mem::take(&mut state.dials)
				.into_iter()
				.partition(|request| request.demand().is_wanted());
			state.dials = open;
			resets.extend(abandoned.into_iter().map(|request| (request, Error::Cancel)));
		}
		for request in taken {
			// `None` for a name naming no dial to this endpoint: outside the scheme, this
			// endpoint's own, or for another acceptor.
			let target = match parse(request.name()) {
				Some(name) if name.acceptor == &*self.id && name.dialer != &*self.id => Some(name.protocol == PROTOCOL),
				_ => None,
			};
			match target {
				// Taken while the endpoint closed: reset like the dials it queued.
				_ if state.end.is_some() => resets.push((request, Error::Cancel)),
				Some(true) => {
					state.dials.push_back(request);
					wakes.push(state.waiting.take());
				}
				Some(false) => resets.push((request, Error::Unsupported)),
				None => resets.push((request, Error::NotFound)),
			}
		}
		let next = match &state.end {
			Some(err) => Poll::Ready(Err(err.clone())),
			None => match state.dials.pop_front() {
				Some(request) => Poll::Ready(Ok(request)),
				None => {
					state.waiting.register(waiter);
					Poll::Pending
				}
			},
		};
		drop(state);
		if let Some((handler, server)) = retired {
			// Closed first, as `close` may not have yet: dropping the last handler rejects
			// the dials still queued with `Dropped`, rather than the close's `Unroutable`.
			if let Some(server) = &server {
				server.broadcast().close();
			}
			drop(handler);
		}
		for mut wake in wakes {
			wake.wake();
		}
		for (request, err) in resets {
			// Not the name: a session's name is its secret.
			tracing::debug!(%err, "reset a request for no session");
			request.reject(err);
		}
		next
	}

	/// Fail with [`Error::Unsupported`] while a session withholds this endpoint's broadcast
	/// from its peer, so no peer beyond it can find it: a moq-lite client session before
	/// 07, whose peer can't ask for hidden broadcasts.
	fn poll_reachable(&self, waiter: &kio::Waiter) -> Result<(), Error> {
		match self.withheld.poll(waiter) {
			Poll::Ready(()) => Err(Error::Unsupported),
			Poll::Pending => Ok(()),
		}
	}

	/// Poll for the endpoint's end, registering `waiter` for it.
	fn poll_end(&self, waiter: &kio::Waiter) -> Result<(), Error> {
		let routes = {
			let state = self.state.lock();
			if let Some(err) = &state.end {
				return Err(err.clone());
			}
			[state.client.clone(), state.server.clone()]
		};
		for route in routes.iter().flatten() {
			if route.broadcast().poll_closed(waiter).is_ready() {
				return Err(Error::Closed);
			}
		}
		Ok(())
	}

	fn close(&self) {
		let mut guard = self.state.lock();
		let state = &mut *guard;
		state.end.get_or_insert(Error::Closed);
		let mut wakes = Vec::new();
		// A poller holding the handler drops it once it sees the end.
		let requests = match std::mem::replace(&mut state.handler, Handler::None) {
			Handler::Idle(requests) => Some(requests),
			Handler::Taken(mut waiting) => {
				wakes.push(waiting.take());
				None
			}
			Handler::None => None,
		};
		let queued = std::mem::take(&mut state.dials);
		wakes.push(state.waiting.take());
		let streams = std::mem::take(&mut state.streams);
		let routes = [state.client.take(), state.server.take()];
		drop(guard);

		// Reset outside the lock, as the session closing, and before retracting the
		// broadcasts: a peer reading through a retracted route sees `Unroutable` instead.
		for stream in streams {
			stream.abort(Error::Stream(StreamError::Session(SessionError::Cancel)));
		}
		for route in routes.iter().flatten() {
			route.broadcast().close();
		}
		for request in queued {
			request.reject(Error::Cancel);
		}
		for mut wake in wakes {
			wake.wake();
		}
		drop(requests);
		drop(routes);
	}
}

/// A socket endpoint: a server, a client, or both.
///
/// Its first dial announces `<prefix>/.client/<id>`, which holds its half of each session
/// it dials, and its first accept announces `<prefix>/.server/<id>`, where peers dial it and
/// which holds its half of each session it accepts. So a pure client never announces a
/// server broadcast, and the origin's grants decide which roles it may take.
///
/// It accepts dials from its first accept on, like a TCP listener, and dials queue until
/// accepted. Until then it has no server broadcast, so a dial to it waits as for a peer
/// not online yet.
///
/// Dropping it, or [`Self::close`], retracts its broadcasts, resets the dials waiting to be
/// accepted, and resets this side's half of every session it handed out. The peer's half
/// of a [`TrackStream`] is read through
/// its [`TrackStream::consumer`], which the application owns: drop the streams too, which
/// releases the subscriptions.
///
/// Every hop between two endpoints must carry hidden broadcasts: moq-lite-07 or
/// moq-transport. While the origin has a moq-lite client session before 07, whose peer
/// never hears the endpoint's broadcast, dials and accepts fail with
/// [`Error::Unsupported`]; a session whose publish half opts into hidden broadcasts
/// ([`origin::Consumer::with_hidden`]) carries it and doesn't count. Nothing tells a
/// dialer that the peer sits behind such a hop, so a dial to it waits as for a peer not
/// announced yet, while the peer's own accepts fail.
pub struct Endpoint {
	shared: Arc<Shared>,
}

impl Endpoint {
	/// The endpoint `id` under `prefix` on `origin`, which also reaches the peers. Nothing
	/// is announced until its first dial or accept.
	///
	/// `id` names the endpoint: a well-known name for a server, or a random one for a
	/// client. It must be one path segment.
	pub fn new(
		origin: &origin::Producer,
		prefix: impl TryInto<Path, Error: Into<Error>>,
		id: &str,
	) -> Result<Self, Error> {
		let prefix = crate::path::convert(prefix)?;
		for role in [Role::Server, Role::Client] {
			endpoint_path(&prefix, role, id)?;
		}
		let consumer = origin.consume();
		Ok(Self {
			shared: Arc::new(Shared {
				id: id.into(),
				prefix,
				publisher: origin.clone(),
				withheld: consumer.hidden_withheld(),
				origin: consumer,
				state: kio::Lock::new(State {
					client: None,
					server: None,
					handler: Handler::None,
					dials: VecDeque::new(),
					waiting: kio::WaiterList::new(),
					streams: Vec::new(),
					end: None,
				}),
			}),
		})
	}

	/// This endpoint's id.
	pub fn id(&self) -> &str {
		&self.shared.id
	}

	/// The prefix the endpoint's broadcasts sit under, and where it finds its peers.
	pub fn prefix(&self) -> &Path {
		&self.shared.prefix
	}

	/// Dial the server `peer` in a new session, resolving once the peer accepts it.
	///
	/// `info` describes this side's half; `None` keeps every message until read, with no
	/// max age.
	///
	/// Waits for as long as the peer takes to accept: drop the returned future to give up,
	/// which resets the session. That includes waiting for the peer's server broadcast to
	/// be announced. Fails with the peer's reset when it rejects the dial, or does not
	/// accept, and with [`Error::Unsupported`] for a dial to this endpoint itself or across
	/// a hop that can't carry it (see [`Endpoint`]). The first dial announces the client
	/// broadcast, and fails as [`origin::Producer::route`] does when the origin may not
	/// publish it.
	pub fn dial_track(&self, peer: &str, info: impl Into<Option<track::Info>>) -> Result<kio::Pending<Dialing>, Error> {
		let info = info.into().unwrap_or_else(messages);
		let shared = &self.shared;
		let path = endpoint_path(&shared.prefix, Role::Server, peer)?;
		if peer == &*shared.id {
			return Err(Error::Unsupported);
		}
		let client = shared.client()?;
		let session = session_id();
		let name = track_name(PROTOCOL, &shared.id, peer, &session);
		// This side's half, waiting for the peer to ask for it, which only the peer can:
		// nobody else knows the session.
		let answer = client.broadcast().reserve_unlisted_track(name.clone())?;
		// Followed rather than requested, so a peer not announced yet is waited for.
		let resolving = Box::new(shared.origin.follow(path, None)?);
		Ok(kio::Pending::new(Dialing {
			shared: shared.clone(),
			peer: peer.into(),
			session,
			name,
			info: Some(info),
			demand: answer.demand(),
			answer: Some(Answer::Reserved(answer)),
			step: Step::Resolving(resolving),
		}))
	}

	/// The next dial from a peer; fails with [`Error::Closed`] once the endpoint closed,
	/// and with [`Error::Unsupported`] while no peer can reach it (see [`Endpoint`]).
	///
	/// The first call starts accepting dials. They queue until taken, or until their
	/// dialer gives up, so keep accepting, and accept or reject each one. The first call
	/// also announces the server broadcast, and fails as [`origin::Producer::route`] does
	/// when the origin may not publish it.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_accept_track(&self, waiter: &kio::Waiter) -> Poll<Result<Incoming, Error>> {
		let shared = &self.shared;
		shared.poll_reachable(waiter)?;
		let request = ready!(shared.poll_dial(waiter))?;
		let name = parse(request.name()).expect("sorted as a dial");
		Poll::Ready(Ok(Incoming {
			shared: shared.clone(),
			peer: name.dialer.into(),
			session: name.session.into(),
			request: Some(request),
		}))
	}

	/// Close the endpoint; see [`Endpoint`].
	pub fn close(&self) {
		self.shared.close();
	}
}

impl Drop for Endpoint {
	fn drop(&mut self) {
		self.shared.close();
	}
}

/// A dial from a peer, from [`Endpoint::poll_accept_track`] (or its async `accept_track`):
/// accept or reject it.
///
/// Dropping it rejects it with [`Error::Cancel`].
pub struct Incoming {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	/// The dialer's request for this side's half; taken by an accept or a reject.
	request: Option<track::Request>,
}

impl Incoming {
	/// The id the dialer claims. An accept checks it: the dial connects only if that
	/// endpoint really dialed this session to this endpoint.
	pub fn peer(&self) -> &str {
		&self.peer
	}

	/// The session's id, which the dialer chose.
	pub fn session(&self) -> &str {
		&self.session
	}

	/// Reject the dial, resetting the dialer's request with [`Error::NotFound`], as for a
	/// session that does not exist.
	pub fn reject(mut self) {
		if let Some(request) = self.request.take() {
			request.reject(Error::NotFound);
		}
	}

	/// Accept the dial, resolving once the dialer answers, which proves it dialed.
	///
	/// `info` describes this side's half, as for [`Endpoint::dial_track`]. Nothing is
	/// written before the dialer answers.
	pub fn accept(mut self, info: impl Into<Option<track::Info>>) -> kio::Pending<Accepting> {
		let info = info.into().unwrap_or_else(messages);
		let shared = self.shared.clone();
		let (peer, session) = (self.peer.clone(), self.session.clone());
		let request = self.request.take().expect("taken only by accept, reject or drop");
		// Followed rather than requested: the dialer announced its client broadcast before
		// it dialed, but a relay can deliver the dial first. A dial claiming an endpoint
		// nobody announces waits for as long as its dialer holds it open.
		let step = match endpoint_path(&shared.prefix, Role::Client, &peer)
			.and_then(|path| shared.origin.follow(path, None))
		{
			Ok(following) => Step::Resolving(Box::new(following)),
			Err(err) => Step::Failed(err),
		};
		kio::Pending::new(Accepting {
			shared: shared.clone(),
			name: track_name(PROTOCOL, &peer, &shared.id, &session),
			peer,
			session,
			info: Some(info),
			demand: request.demand(),
			request: Some(request),
			step,
		})
	}
}

impl Drop for Incoming {
	fn drop(&mut self) {
		if let Some(request) = self.request.take() {
			request.reject(Error::Cancel);
		}
	}
}

/// How far a dial or an accept got in reading the peer's half.
enum Step {
	/// Waiting for the peer's endpoint broadcast to be announced.
	Resolving(Box<origin::Following>),
	/// Subscribed to the peer's half, waiting for the peer to serve it.
	Subscribing(track::Subscribing),
	/// The peer serves its half.
	Subscribed(Box<track::Subscriber>),
	/// It failed before it started.
	Failed(Error),
	/// Handed out, or failed.
	Done,
}

impl Step {
	/// Advance until the peer serves its half, subscribing to `track` in its endpoint.
	fn poll(&mut self, waiter: &kio::Waiter, track: &str) -> Poll<Result<(), Error>> {
		loop {
			match self {
				Self::Resolving(following) => {
					// Only the first call is made here, and it never reports the path offline.
					let broadcast = ready!(following.poll_available(waiter))?.expect("the first call is never offline");
					let subscribing = broadcast.track(track)?.subscribe(subscription()).into_inner();
					*self = Self::Subscribing(subscribing);
				}
				Self::Subscribing(subscribing) => {
					let subscriber = ready!(subscribing.poll_ok(waiter))?;
					*self = Self::Subscribed(Box::new(subscriber));
				}
				Self::Subscribed(_) => return Poll::Ready(Ok(())),
				Self::Failed(err) => return Poll::Ready(Err(err.clone())),
				Self::Done => return Poll::Ready(Err(Error::Closed)),
			}
		}
	}

	fn take_subscriber(&mut self) -> track::Subscriber {
		match std::mem::replace(self, Self::Done) {
			Self::Subscribed(subscriber) => *subscriber,
			_ => unreachable!("taken once subscribed"),
		}
	}
}

/// Hand out a session's stream once its handshake is done, unless the endpoint stopped,
/// in which case this side's half is aborted, outside the lock.
fn open(
	shared: &Shared,
	producer: track::Producer,
	step: &mut Step,
	peer: &Arc<str>,
	session: &Arc<str>,
) -> Result<TrackStream, Error> {
	let mut state = shared.state.lock();
	if let Some(err) = state.end.clone() {
		drop(state);
		let _ = producer.abort(err.clone());
		return Err(err);
	}
	state.hold(&producer);
	drop(state);
	Ok(TrackStream {
		producer,
		consumer: step.take_subscriber(),
		peer: peer.clone(),
		session: session.clone(),
	})
}

/// This side's half of a dial.
enum Answer {
	/// Reserved, until the peer asks for it.
	Reserved(track::Request),
	/// Served, once the peer asked.
	Served(track::Producer),
}

impl Answer {
	/// Reset the half for the peer with `err`.
	fn reset(self, err: Error) {
		match self {
			Self::Reserved(request) => request.reject(err),
			Self::Served(producer) => {
				let _ = producer.abort(err);
			}
		}
	}
}

/// A dial, from [`Endpoint::dial_track`]: resolves once the peer accepts.
///
/// Dropping it gives up, resetting the session.
pub struct Dialing {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	/// The session's track name.
	name: String,
	info: Option<track::Info>,
	/// Whether the peer asked for this side's half, which it does on accept.
	demand: track::Demand,
	answer: Option<Answer>,
	step: Step,
}

impl Dialing {
	/// Poll for the peer to accept; see [`Endpoint::dial_track`].
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<TrackStream, Error>> {
		let result = self.poll_inner(waiter);
		if let Poll::Ready(Err(err)) = &result {
			if let Some(answer) = self.answer.take() {
				answer.reset(err.clone());
			}
			self.step = Step::Done;
		}
		result
	}

	fn poll_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<TrackStream, Error>> {
		self.shared.poll_end(waiter)?;
		self.shared.poll_reachable(waiter)?;

		// The peer asks for this side's half when it accepts: serve it, which proves to the
		// peer that this endpoint dialed.
		if let Some(Answer::Reserved(_)) = &self.answer {
			// Polled until it parks, so the next change wakes this.
			while let Poll::Ready(changed) = self.demand.poll_wanted_changed(waiter) {
				// Ends once the reservation does: the endpoint closed.
				changed?;
			}
			if self.demand.wanted().is_some()
				&& let Some(Answer::Reserved(request)) = self.answer.take()
			{
				self.answer = Some(Answer::Served(request.accept(self.info.take())));
			}
		}

		ready!(self.step.poll(waiter, &self.name))?;
		// The peer serves its half only after asking for this side's, so a conforming peer
		// asked already; wait for its request regardless.
		let Some(Answer::Served(_)) = &self.answer else {
			return Poll::Pending;
		};
		let Some(Answer::Served(producer)) = self.answer.take() else {
			unreachable!("checked above");
		};
		Poll::Ready(open(&self.shared, producer, &mut self.step, &self.peer, &self.session))
	}
}

impl kio::Task for Dialing {
	type Output = Result<TrackStream, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

impl Drop for Dialing {
	fn drop(&mut self) {
		// Given up: reset the session for the peer.
		if let Some(answer) = self.answer.take() {
			answer.reset(Error::Cancel);
		}
	}
}

/// An accept, from an [`Incoming`] dial: resolves once the dialer answers.
pub struct Accepting {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	/// The session's track name.
	name: String,
	info: Option<track::Info>,
	/// The dialer's request for this side's half, served once the dialer answers.
	request: Option<track::Request>,
	/// Whether the dialer still subscribes to this side's half: the dial stays open while
	/// it does.
	demand: track::Demand,
	step: Step,
}

impl Accepting {
	/// Poll for the dialer to answer, which proves it dialed.
	///
	/// Waits for the dialer's endpoint to be announced for as long as the dial stays open,
	/// and fails with [`Error::Cancel`] once the dialer gives up, or with
	/// [`Error::Unsupported`] while this endpoint can't reach it (see [`Endpoint`]). Fails
	/// with the dialer's reset when it never dialed this session, such as a dial forged in
	/// its name, and resets the dialer's request with the same error.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<TrackStream, Error>> {
		let result = self.poll_inner(waiter);
		if let Poll::Ready(Err(err)) = &result {
			self.step = Step::Done;
			if let Some(request) = self.request.take() {
				request.reject(err.clone());
			}
		}
		result
	}

	fn poll_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<TrackStream, Error>> {
		self.shared.poll_end(waiter)?;
		self.shared.poll_reachable(waiter)?;
		if self.demand.poll_state(waiter) != track::DemandState::Active {
			return Poll::Ready(Err(Error::Cancel));
		}
		ready!(self.step.poll(waiter, &self.name))?;

		// Accepted outside the lock, since it wakes the dialer.
		let request = self.request.take().ok_or(Error::Closed)?;
		let producer = request.accept(self.info.take());
		Poll::Ready(open(&self.shared, producer, &mut self.step, &self.peer, &self.session))
	}
}

impl kio::Task for Accepting {
	type Output = Result<TrackStream, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

impl Drop for Accepting {
	fn drop(&mut self) {
		// Given up: reset the dialer's request.
		if let Some(request) = self.request.take() {
			request.reject(Error::Cancel);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn track_names_round_trip() {
		let name = track_name("track", "alice", "bob", "0123abcd");
		assert_eq!(name, "track/alice/bob/0123abcd");
		assert_eq!(
			parse(&name),
			Some(Name {
				protocol: "track",
				dialer: "alice",
				acceptor: "bob",
				session: "0123abcd"
			})
		);
	}

	#[test]
	fn foreign_track_names_are_skipped() {
		for name in [
			"command",
			"alice/s",
			"track/alice/s",
			"track/alice/bob/",
			"track//bob/s",
			"track/alice//s",
			"/alice/bob/s",
			"track/alice/bob/s/0",
			"track/a\\b/bob/s",
			"track/alice/bob/s\\",
		] {
			assert_eq!(parse(name), None, "{name}");
		}
	}

	#[test]
	fn an_endpoint_path_holds_one_segment() {
		let prefix = Path::from_static("room");
		assert_eq!(
			endpoint_path(&prefix, Role::Server, "alice").unwrap().as_str(),
			"room/.server/alice"
		);
		assert_eq!(
			endpoint_path(&prefix, Role::Client, "alice").unwrap().as_str(),
			"room/.client/alice"
		);
		for role in [Role::Server, Role::Client] {
			assert!(is_endpoint(&endpoint_path(&prefix, role, "alice").unwrap()));
			assert!(endpoint_path(&prefix, role, "a/b").is_err());
			assert!(endpoint_path(&prefix, role, "").is_err());
			assert!(endpoint_path(&prefix, role, "a\\b").is_err());
		}
		for path in [
			"room/alice",
			".server",
			"room/.server",
			"room/.server/alice/cam",
			"room/server/alice",
		] {
			assert!(!is_endpoint(&Path::new(path).unwrap()), "{path}");
		}
	}

	/// Taking a dial wakes the dialer's subscription, and an inline executor can re-enter
	/// the endpoint from that wake, such as to accept again or to close it, so the endpoint
	/// polls its handler with no lock held.
	#[test]
	fn taking_a_dial_wakes_outside_the_lock() {
		use std::sync::atomic::{AtomicBool, Ordering};
		use std::task::Wake;

		struct Close {
			shared: Arc<Shared>,
			woken: AtomicBool,
		}
		impl Wake for Close {
			fn wake(self: Arc<Self>) {
				// The handler is out being polled, so this waits for it.
				let accept = self.shared.poll_dial(&kio::Waiter::noop());
				assert!(accept.is_pending());
				self.shared.close();
				self.woken.store(true, Ordering::SeqCst);
			}
		}

		let (done, finished) = std::sync::mpsc::channel();
		let thread = std::thread::spawn(move || {
			let (origin, _driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
			let endpoint = Endpoint::new(&origin, "room", "bob").unwrap();
			let noop = kio::Waiter::noop();
			// Listening, so the dial queues for the handler.
			assert!(endpoint.poll_accept_track(&noop).is_pending());

			let server = endpoint.shared.state.lock().server.clone().unwrap();
			let broadcast = server.broadcast().consume();
			let dial = broadcast
				.track("track/alice/bob/0123456789abcdef0123456789abcdef")
				.unwrap()
				.subscribe(None)
				.into_inner();
			let close = Arc::new(Close {
				shared: endpoint.shared.clone(),
				woken: AtomicBool::new(false),
			});
			let waiter = kio::Waiter::new(std::task::Waker::from(close.clone()));
			assert!(dial.poll_ok(&waiter).is_pending());

			// The wake closes the endpoint while this takes the dial, which it resets.
			assert!(matches!(
				endpoint.poll_accept_track(&noop),
				Poll::Ready(Err(Error::Closed))
			));
			assert!(close.woken.load(Ordering::SeqCst));
			assert!(matches!(dial.poll_ok(&noop), Poll::Ready(Err(_))));
			done.send(()).unwrap();
		});
		let timeout = std::time::Duration::from_secs(10);
		if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(timeout) {
			panic!("deadlocked re-entering the endpoint");
		}
		thread.join().unwrap();
	}

	/// A dial whose dialer gave up leaves the queue, so an accept never takes it, and
	/// dialing and giving up again never piles up requests.
	#[test]
	fn abandoned_dials_leave_the_queue() {
		let (origin, _driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
		let endpoint = Endpoint::new(&origin, "room", "bob").unwrap();
		let noop = kio::Waiter::noop();
		assert!(endpoint.poll_accept_track(&noop).is_pending());

		let server = endpoint.shared.state.lock().server.clone().unwrap();
		let broadcast = server.broadcast().consume();
		let queued = || endpoint.shared.state.lock().dials.len();
		let dial = |session: u32| {
			let dial = broadcast
				.track(&format!("track/alice/bob/{session:032x}"))
				.unwrap()
				.subscribe(None)
				.into_inner();
			assert!(dial.poll_ok(&noop).is_pending());
			dial
		};
		let accept = || match endpoint.poll_accept_track(&noop) {
			Poll::Ready(Ok(incoming)) => incoming.session().to_string(),
			_ => panic!("no dial to accept"),
		};

		// The accept takes both dials on hand and queues the second.
		let _first = dial(0);
		let mut waiting = dial(1);
		assert_eq!(accept(), format!("{:032x}", 0));
		for session in (2..8).step_by(2) {
			assert_eq!(queued(), 1, "dial {session}");
			drop(waiting);
			let _taken = dial(session);
			waiting = dial(session + 1);
			assert_eq!(accept(), format!("{session:032x}"));
		}
		assert_eq!(queued(), 1);
	}

	#[test]
	fn session_ids_are_128_bits() {
		let (a, b) = (session_id(), session_id());
		assert_eq!(a.len(), 32);
		assert_ne!(a, b);
	}
}
