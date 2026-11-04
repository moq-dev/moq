//! Point-to-point sockets over an origin, the way TCP connects two hosts.
//!
//! An [`Endpoint`] announces one hidden broadcast, `<prefix>/.tcp/<id>`, and from it
//! [`dial`](Endpoint::dial)s other endpoints and [`accept`](Endpoint::accept)s their
//! dials. Each connection is a session: one [`Stream`], a reliable, ordered pipe made of
//! two tracks, one in each endpoint's broadcast. Connections are never announced.
//!
//! A dialer `A` picks a random session id `S` and subscribes to `<prefix>/.tcp/<B>` for
//! the track `A/S`. Endpoint `B` surfaces that request as an [`Incoming`] dial from `A`.
//! To accept it, `B` subscribes to `<prefix>/.tcp/<A>` for the same track name, and `A`
//! answers only a session it dialed, so the exchange checks that `A` really asked: a
//! dial forged in `A`'s name is reset. Each side then writes its own half and reads the
//! peer's. Session tracks are served only on request, so no track listing names them.
//!
//! See `doc/concept/socket.md` for the convention other languages follow.

use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, ready};

use crate::{Error, Path};

use super::WeakEntry;
use super::origin::{self, Route};
use super::track::{self, Delay, Order, Subscription};

/// The segment an endpoint's broadcast sits under: one reliable, ordered pipe per
/// session, as TCP gives. A multiplexed mode would take a segment of its own.
const TCP: &str = ".tcp";

/// Refuse an id that is not one plain path segment: empty, or holding a `/` or a `\`.
fn check_id(id: &str) -> Result<(), Error> {
	if id.is_empty() || id.contains(['/', '\\']) {
		return Err(crate::InvalidPattern::InvalidSegment(id.to_string()).into());
	}
	Ok(())
}

/// `<prefix>/.tcp/<id>`, the broadcast of the endpoint `id`.
fn endpoint_path(prefix: &Path, id: &str) -> Result<Path, Error> {
	check_id(id)?;
	Ok(prefix.join(&Path::from_static(TCP))?.join(&Path::new(id)?)?)
}

/// A fresh session id: 128 random bits as hex, so nobody can guess a session's tracks.
fn session_id() -> Arc<str> {
	use rand::RngExt;
	format!("{:032x}", rand::rng().random::<u128>()).into()
}

/// The track name of a session: `<dialer>/<session>`, the same in both endpoints.
fn track_name(dialer: &str, session: &str) -> String {
	format!("{dialer}/{session}")
}

/// Split a session's track name into its dialer and session, or `None` for a name
/// outside the scheme.
fn parse(name: &str) -> Option<(&str, &str)> {
	let (dialer, session) = name.split_once('/')?;
	check_id(dialer).ok()?;
	check_id(session).ok()?;
	Some((dialer, session))
}

/// What a session's reader asks for: every message, oldest first, however late it is.
fn reliable() -> Subscription {
	Subscription::default()
		.with_order(Order::Ascending)
		.with_delay(Delay::default().with_max(None))
}

/// One session: this side's half and the peer's.
///
/// Each message is one group, so a slow message never holds up the next one, and messages
/// are ordered by group sequence. [`Self::consumer`] starts reliable and in order (oldest
/// first, with no delay budget); update its subscription to make it lossy.
///
/// Finishing [`Self::producer`] closes the session for the peer, and aborting it is an
/// error. The endpoint closing aborts it too.
pub struct Stream {
	/// This side's half: the session's track in this endpoint's broadcast.
	pub producer: track::Producer,
	/// The peer's half: the session's track in the peer's broadcast.
	pub consumer: track::Subscriber,
	peer: Arc<str>,
	session: Arc<str>,
}

impl Stream {
	/// The peer endpoint's id.
	pub fn peer(&self) -> &str {
		&self.peer
	}

	/// The session's id, which the dialer chose.
	pub fn session(&self) -> &str {
		&self.session
	}
}

/// What the endpoint's pollers share.
struct State {
	/// The dials peers send, from the first accept on; `None` until then, so the endpoint
	/// answers a request for a session it does not hold with [`Error::NotFound`] at once.
	requests: Option<super::broadcast::Requests>,
	/// Dials from peers, not yet taken by an accept, in order.
	incoming: VecDeque<track::Request>,
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

struct Shared {
	id: Arc<str>,
	prefix: Path,
	/// Where the peers' endpoints are requested.
	origin: origin::Consumer,
	/// This endpoint's broadcast.
	publication: origin::Publication,
	state: kio::Lock<State>,
}

impl Shared {
	/// Queue every dial on hand and take the next, registering `waiter` for more, and
	/// fail with the endpoint's end once it stopped.
	///
	/// A dial of this endpoint's reserves its own track, so a request reaching the handler
	/// under this endpoint's id names a session it never dialed, or one it gave up, such
	/// as an accept of a dial forged in this endpoint's name: it is reset, which fails
	/// that accept. Resets go out after the lock is released, since they wake readers.
	fn poll_dial(&self, waiter: &kio::Waiter) -> Poll<Result<track::Request, Error>> {
		let mut resets = Vec::new();
		let mut state = self.state.lock();
		while state.end.is_none() {
			let requests = state.requests.get_or_insert_with(|| self.publication.requests());
			match requests.poll_requested_track(waiter) {
				Poll::Pending => break,
				Poll::Ready(Ok(request)) => match parse(request.name()) {
					Some((dialer, _)) if dialer != &*self.id => state.incoming.push_back(request),
					_ => resets.push(request),
				},
				Poll::Ready(Err(err)) => state.end = Some(err),
			}
		}
		let next = match &state.end {
			Some(err) => Poll::Ready(Err(err.clone())),
			None => match state.incoming.pop_front() {
				Some(request) => Poll::Ready(Ok(request)),
				None => Poll::Pending,
			},
		};
		drop(state);
		for request in resets {
			// Not the name: a session's name is its secret.
			tracing::debug!("reset a request for no session");
			request.reject(Error::NotFound);
		}
		next
	}

	/// Poll for the endpoint's end, registering `waiter` for it.
	fn poll_end(&self, waiter: &kio::Waiter) -> Result<(), Error> {
		if let Some(err) = &self.state.lock().end {
			return Err(err.clone());
		}
		match self.publication.poll_closed(waiter) {
			Poll::Ready(()) => Err(Error::Closed),
			Poll::Pending => Ok(()),
		}
	}

	fn close(&self) {
		let mut state = self.state.lock();
		state.end.get_or_insert(Error::Closed);
		let requests = state.requests.take();
		let incoming = std::mem::take(&mut state.incoming);
		let streams = std::mem::take(&mut state.streams);
		drop(state);

		self.publication.close();
		for stream in streams {
			stream.abort(Error::Cancel);
		}
		// Reset outside the lock.
		drop((requests, incoming));
	}
}

/// A socket endpoint: announces `<prefix>/.tcp/<id>`, dials other endpoints, and accepts
/// their dials.
///
/// It accepts dials from its first [`Self::accept`] on, like a TCP listener: before that, a
/// dial to it is reset, and after it, dials queue until accepted. Dropping it, or
/// [`Self::close`], retracts the broadcast, resets the dials waiting to be accepted, and
/// aborts every session it handed out.
pub struct Endpoint {
	shared: Arc<Shared>,
}

impl Endpoint {
	/// Announce the endpoint `id` under `prefix` on `origin`, which also reaches the peers.
	///
	/// `id` names the endpoint: a well-known name for a server, or a random one for a
	/// client. It must be one path segment. Fails like
	/// [`origin::Producer::create_broadcast`] when `origin` may not publish
	/// `<prefix>/.tcp/<id>`.
	pub fn new(
		origin: &origin::Producer,
		prefix: impl TryInto<Path, Error: Into<Error>>,
		id: &str,
	) -> Result<Self, Error> {
		let prefix = crate::path::convert(prefix)?;
		let publication = origin.create_broadcast(endpoint_path(&prefix, id)?)?;
		publication.announce(Route::default())?;
		Ok(Self {
			shared: Arc::new(Shared {
				id: id.into(),
				prefix,
				origin: origin.consume(),
				publication,
				state: kio::Lock::new(State {
					requests: None,
					incoming: VecDeque::new(),
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

	/// The prefix the endpoint's broadcast sits under, and where it finds its peers.
	pub fn prefix(&self) -> &Path {
		&self.shared.prefix
	}

	/// Dial the endpoint `peer` in a new session, resolving once the peer accepts it.
	///
	/// `info` describes this side's half. Waits for as long as the peer takes to accept:
	/// drop the returned future to give up, which resets the session. That includes waiting
	/// for the peer's endpoint to be announced. Fails with the peer's reset when it rejects
	/// the dial, and with [`Error::Unsupported`] for a dial to this endpoint itself.
	pub fn dial(&self, peer: &str, info: impl Into<Option<track::Info>>) -> Result<kio::Pending<Dialing>, Error> {
		let shared = &self.shared;
		let path = endpoint_path(&shared.prefix, peer)?;
		if peer == &*shared.id {
			return Err(Error::Unsupported);
		}
		if let Some(err) = &shared.state.lock().end {
			return Err(err.clone());
		}
		let session = session_id();
		// This side's half, waiting for the peer to ask for it, which only the peer can:
		// nobody else knows the session.
		let answer = shared
			.publication
			.reserve_unlisted_track(track_name(&shared.id, &session))?;
		// Followed rather than requested, so a peer not announced yet is waited for.
		let resolving = Resolving::Follow(shared.origin.follow(path, None)?);
		Ok(kio::Pending::new(Dialing {
			shared: shared.clone(),
			peer: peer.into(),
			session,
			info: info.into(),
			demand: answer.demand(),
			answer: Some(Answer::Reserved(answer)),
			step: Step::Resolving(resolving),
		}))
	}

	/// Poll for the next dial from a peer; fails with [`Error::Closed`] once the endpoint
	/// closed.
	///
	/// Dials queue until taken, without limit, each holding the dialer's request open, so
	/// keep accepting, and accept or reject each one.
	pub fn poll_accept(&self, waiter: &kio::Waiter) -> Poll<Result<Incoming, Error>> {
		let shared = &self.shared;
		let request = ready!(shared.poll_dial(waiter))?;
		let (peer, session) = parse(request.name()).expect("sorted as a dial");
		Poll::Ready(Ok(Incoming {
			shared: shared.clone(),
			peer: peer.into(),
			session: session.into(),
			request,
		}))
	}

	/// Wait for the next dial from a peer; see [`Self::poll_accept`].
	#[cfg(feature = "async")]
	pub async fn accept(&self) -> Result<Incoming, Error> {
		kio::wait(|waiter| self.poll_accept(waiter)).await
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

/// A dial from a peer, from [`Endpoint::accept`]: accept or reject it.
///
/// Dropping it rejects it.
pub struct Incoming {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	request: track::Request,
}

impl Incoming {
	/// The id the dialer claims. [`Self::accept`] checks it: the dial connects only if that
	/// endpoint really dialed this session.
	pub fn peer(&self) -> &str {
		&self.peer
	}

	/// The session's id, which the dialer chose.
	pub fn session(&self) -> &str {
		&self.session
	}

	/// Accept the dial, resolving once the dialer answers, which proves it dialed.
	///
	/// `info` describes this side's half. Nothing is written before the dialer answers.
	pub fn accept(self, info: impl Into<Option<track::Info>>) -> kio::Pending<Accepting> {
		let Self {
			shared,
			peer,
			session,
			request,
		} = self;
		// Requested, so a dial claiming an endpoint nobody announces fails at once: the
		// dialer announced its endpoint before it dialed.
		let resolving = endpoint_path(&shared.prefix, &peer)
			.map(|path| Resolving::Request(shared.origin.request(path, None).into_inner()));
		let step = match resolving {
			Ok(resolving) => Step::Resolving(resolving),
			Err(err) => Step::Failed(err),
		};
		kio::Pending::new(Accepting {
			shared,
			peer,
			session,
			info: info.into(),
			request: Some(request),
			step,
		})
	}

	/// Reject the dial, resetting the dialer's request with [`Error::NotFound`], as for a
	/// session that does not exist.
	pub fn reject(self) {
		self.request.reject(Error::NotFound);
	}
}

/// Resolving the peer's endpoint broadcast.
enum Resolving {
	/// Failing once the endpoint is not announced.
	Request(origin::Requesting),
	/// Waiting for the endpoint to be announced.
	Follow(origin::Following),
}

impl Resolving {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<super::broadcast::Consumer, Error>> {
		match self {
			Self::Request(request) => request.poll_ok(waiter),
			Self::Follow(follow) => follow.poll_available(waiter),
		}
	}
}

/// How far a dial or an accept got in reading the peer's half.
enum Step {
	/// Resolving the peer's endpoint broadcast.
	Resolving(Resolving),
	/// Subscribed to the peer's half, waiting for the peer to serve it.
	Subscribing(track::Subscribing),
	/// The peer serves its half.
	Subscribed(track::Subscriber),
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
				Self::Resolving(resolving) => {
					let broadcast = ready!(resolving.poll(waiter))?;
					let subscribing = broadcast.track(track)?.subscribe(reliable()).into_inner();
					*self = Self::Subscribing(subscribing);
				}
				Self::Subscribing(subscribing) => {
					let subscriber = ready!(subscribing.poll_ok(waiter))?;
					*self = Self::Subscribed(subscriber);
				}
				Self::Subscribed(_) => return Poll::Ready(Ok(())),
				Self::Failed(err) => return Poll::Ready(Err(err.clone())),
				Self::Done => return Poll::Ready(Err(Error::Closed)),
			}
		}
	}

	fn take_subscriber(&mut self) -> track::Subscriber {
		match std::mem::replace(self, Self::Done) {
			Self::Subscribed(subscriber) => subscriber,
			_ => unreachable!("taken once subscribed"),
		}
	}
}

/// This side's half of a dial.
enum Answer {
	/// Reserved, until the peer asks for it.
	Reserved(track::Request),
	/// Served, once the peer asked.
	Served(track::Producer),
}

/// A dial, from [`Endpoint::dial`]: resolves once the peer accepts.
///
/// Dropping it gives up, resetting the session.
pub struct Dialing {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	info: Option<track::Info>,
	/// Whether the peer asked for this side's half, which it does on accept.
	demand: track::Demand,
	answer: Option<Answer>,
	step: Step,
}

impl Dialing {
	/// The session's id.
	pub fn session(&self) -> &str {
		&self.session
	}

	/// Poll for the peer to accept; see [`Endpoint::dial`].
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<Stream, Error>> {
		let result = self.poll_inner(waiter);
		if let Poll::Ready(Err(_)) = &result {
			// Resets the session for the peer.
			self.answer = None;
			self.step = Step::Done;
		}
		result
	}

	fn poll_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<Stream, Error>> {
		let shared = &self.shared;
		shared.poll_end(waiter)?;

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

		let track = track_name(&shared.id, &self.session);
		ready!(self.step.poll(waiter, &track))?;
		// The peer serves its half only after asking for this side's, so a conforming peer
		// asked already; wait for its request regardless.
		let Some(Answer::Served(_)) = &self.answer else {
			return Poll::Pending;
		};
		let Some(Answer::Served(producer)) = self.answer.take() else {
			unreachable!("checked above");
		};

		let mut state = shared.state.lock();
		if let Some(err) = state.end.clone() {
			// Aborted outside the lock.
			drop(state);
			let _ = producer.abort(err.clone());
			return Poll::Ready(Err(err));
		}
		state.hold(&producer);
		drop(state);
		Poll::Ready(Ok(Stream {
			producer,
			consumer: self.step.take_subscriber(),
			peer: self.peer.clone(),
			session: self.session.clone(),
		}))
	}
}

impl kio::Task for Dialing {
	type Output = Result<Stream, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// An accept, from [`Incoming::accept`]: resolves once the dialer answers.
pub struct Accepting {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	info: Option<track::Info>,
	/// The dialer's request for this side's half, served once the dialer answers.
	request: Option<track::Request>,
	step: Step,
}

impl Accepting {
	/// Poll for the dialer to answer; see [`Incoming::accept`].
	///
	/// Fails with the dialer's reset when it never dialed this session, such as a dial
	/// forged in its name, and resets the dialer's request with the same error.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<Stream, Error>> {
		let result = self.poll_inner(waiter);
		if let Poll::Ready(Err(err)) = &result {
			self.step = Step::Done;
			if let Some(request) = self.request.take() {
				request.reject(err.clone());
			}
		}
		result
	}

	fn poll_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<Stream, Error>> {
		let shared = &self.shared;
		shared.poll_end(waiter)?;
		let track = track_name(&self.peer, &self.session);
		ready!(self.step.poll(waiter, &track))?;

		// Accepted outside the lock, since it wakes the dialer.
		let request = self.request.take().ok_or(Error::Closed)?;
		let producer = request.accept(self.info.take());
		let mut state = shared.state.lock();
		if let Some(err) = state.end.clone() {
			drop(state);
			let _ = producer.abort(err.clone());
			return Poll::Ready(Err(err));
		}
		state.hold(&producer);
		drop(state);
		Poll::Ready(Ok(Stream {
			producer,
			consumer: self.step.take_subscriber(),
			peer: self.peer.clone(),
			session: self.session.clone(),
		}))
	}
}

impl kio::Task for Accepting {
	type Output = Result<Stream, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn track_names_round_trip() {
		let name = track_name("alice", "0123abcd");
		assert_eq!(name, "alice/0123abcd");
		assert_eq!(parse(&name), Some(("alice", "0123abcd")));
	}

	#[test]
	fn foreign_track_names_are_skipped() {
		for name in ["command", "alice/", "/s", "alice/s/0", "a\\b/s", "alice/s\\"] {
			assert_eq!(parse(name), None, "{name}");
		}
	}

	#[test]
	fn an_endpoint_path_holds_one_segment() {
		let prefix = Path::from_static("room");
		assert_eq!(endpoint_path(&prefix, "alice").unwrap().as_str(), "room/.tcp/alice");
		assert!(endpoint_path(&prefix, "a/b").is_err());
		assert!(endpoint_path(&prefix, "").is_err());
		assert!(endpoint_path(&prefix, "a\\b").is_err());
	}

	#[test]
	fn session_ids_are_128_bits() {
		let (a, b) = (session_id(), session_id());
		assert_eq!(a.len(), 32);
		assert_ne!(a, b);
	}
}
