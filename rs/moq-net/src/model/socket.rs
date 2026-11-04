//! Point-to-point sessions over an origin, the way TCP connects two hosts.
//!
//! An [`Endpoint`] announces one hidden broadcast, `<prefix>/.socket/<id>`, used only to
//! route dials to it. From it, an endpoint dials other endpoints and accepts their dials.
//! Each connection is a session: two tracks, one in each endpoint's broadcast, both named
//! `<protocol>/<dialer>/<session>`. Sessions are never announced or listed.
//!
//! A session speaks one of two protocols:
//!
//! - `tcp`: one reliable, ordered byte pipe each way, as TCP gives. Each half is a single
//!   group. Dial with [`Endpoint::dial_tcp`], accept with [`Endpoint::accept_tcp`], and
//!   read and write a [`TcpStream`].
//! - `track`: each half is a whole track, so messages are groups, with the track's own
//!   priority, delay budget and order. Dial with [`Endpoint::dial_track`], accept with
//!   [`Endpoint::accept_track`], and read and write a [`TrackStream`].
//!
//! A dialer `A` picks a random session id `S` and subscribes to `<prefix>/.socket/<B>` for
//! the track `<protocol>/A/S`. Endpoint `B` surfaces that request as an [`Incoming`] dial
//! from `A` in its protocol's queue. To accept it, `B` subscribes to `<prefix>/.socket/<A>`
//! for the same track name, and `A` answers only a session it dialed, so the exchange
//! checks that `A` really asked: a dial forged in `A`'s name is reset. Each side then
//! writes its own half and reads the peer's.
//!
//! See `doc/concept/socket.md` for the convention other languages follow.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Poll, ready};

use crate::{Error, Path};

use super::WeakEntry;
use super::origin::{self, Route};
use super::track::{self, Delay, Order, Subscription};
use super::{frame, group};

/// The segment every endpoint's broadcast sits under.
const SOCKET: &str = ".socket";

/// Refuse an id that is not one plain path segment: empty, or holding a `/` or a `\`.
fn check_id(id: &str) -> Result<(), Error> {
	if id.is_empty() || id.contains(['/', '\\']) {
		return Err(crate::InvalidPattern::InvalidSegment(id.to_string()).into());
	}
	Ok(())
}

/// `<prefix>/.socket/<id>`, the broadcast of the endpoint `id`.
fn endpoint_path(prefix: &Path, id: &str) -> Result<Path, Error> {
	check_id(id)?;
	Ok(prefix.join(&Path::from_static(SOCKET))?.join(&Path::new(id)?)?)
}

/// A fresh session id: 128 random bits as hex, so nobody can guess a session's tracks.
fn session_id() -> Arc<str> {
	use rand::RngExt;
	format!("{:032x}", rand::rng().random::<u128>()).into()
}

/// The track name of a session: `<protocol>/<dialer>/<session>`, the same in both
/// endpoints.
fn track_name(protocol: &str, dialer: &str, session: &str) -> String {
	format!("{protocol}/{dialer}/{session}")
}

/// A session's track name, split.
#[derive(Debug, PartialEq, Eq)]
struct Name<'a> {
	protocol: &'a str,
	dialer: &'a str,
	session: &'a str,
}

/// Split a session's track name, or `None` for a name outside the scheme.
fn parse(name: &str) -> Option<Name<'_>> {
	let (protocol, rest) = name.split_once('/')?;
	let (dialer, session) = rest.split_once('/')?;
	for segment in [protocol, dialer, session] {
		check_id(segment).ok()?;
	}
	Some(Name {
		protocol,
		dialer,
		session,
	})
}

/// What a session's reader asks for: every group, oldest first, however late it is.
fn reliable() -> Subscription {
	Subscription::default()
		.with_order(Order::Ascending)
		.with_delay(Delay::default().with_max(None))
}

mod sealed {
	use super::*;

	/// The protocols an endpoint speaks.
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	pub enum Protocol {
		Tcp,
		Track,
	}

	/// What an endpoint's handles share.
	pub struct Shared {
		pub(super) id: Arc<str>,
		pub(super) prefix: Path,
		/// Where the peers' endpoints are requested.
		pub(super) origin: origin::Consumer,
		/// Where the `tcp` sessions' pumps run: the origin's driver.
		pub(super) tasks: crate::util::TasksWeak,
		/// This endpoint's broadcast.
		pub(super) publication: origin::Publication,
		pub(super) state: kio::Lock<State>,
	}

	/// A session's two halves, once the handshake is done.
	pub struct Parts {
		pub shared: Arc<Shared>,
		/// This side's half, served to the peer.
		pub producer: track::Producer,
		/// The peer's half, subscribed reliably and in order.
		pub subscriber: track::Subscriber,
		pub peer: Arc<str>,
		pub session: Arc<str>,
	}

	/// What a protocol does with a session's halves.
	pub trait Open: Sized {
		/// The protocol's segment, the first of a session's track name.
		const PROTOCOL: Protocol;

		/// Make the stream an application reads and writes.
		fn open(parts: Parts) -> Self;
	}
}

use sealed::{Parts, Protocol, Shared};

impl Protocol {
	fn as_str(self) -> &'static str {
		match self {
			Self::Tcp => "tcp",
			Self::Track => "track",
		}
	}

	fn parse(protocol: &str) -> Option<Self> {
		match protocol {
			"tcp" => Some(Self::Tcp),
			"track" => Some(Self::Track),
			_ => None,
		}
	}
}

/// A session's stream: a [`TcpStream`] or a [`TrackStream`]. Names the protocol a dial,
/// an incoming dial, or an accept is for.
pub trait Stream: sealed::Open {}

/// A `tcp` session: one reliable, ordered byte pipe each way, as TCP gives.
///
/// Each half is the session track's only group, which is one QUIC stream on every hop,
/// so it is reliable, ordered and flow controlled like a TCP direction. A frame is a
/// write. Finishing [`Self::producer`] is FIN, and aborting or dropping it is RST; the
/// peer reads the same from [`Self::consumer`]. Reading ends once the peer finished, and
/// fails with its reset. A second group on a peer's half is a protocol error that resets
/// the session.
///
/// Await [`group::Producer::poll_ready`] (or `ready`) before each write: it waits until the peer reads, so
/// the writer runs at most a window ahead of the reader and nothing is lost. A writer
/// that never waits sheds the front of its half, and a reader that falls behind it fails
/// with [`Error::Evicted`].
///
/// Dropping [`Self::consumer`] before the peer finished resets the session, since nobody
/// reads what the peer writes. The endpoint closing resets it too.
pub struct TcpStream {
	/// This side's half: the frames written here reach the peer in order.
	pub producer: group::Producer,
	/// The peer's half: its frames, in order.
	pub consumer: group::Consumer,
	peer: Arc<str>,
	session: Arc<str>,
}

impl TcpStream {
	/// The peer endpoint's id.
	pub fn peer(&self) -> &str {
		&self.peer
	}

	/// The session's id, which the dialer chose.
	pub fn session(&self) -> &str {
		&self.session
	}
}

impl Stream for TcpStream {}

impl sealed::Open for TcpStream {
	const PROTOCOL: Protocol = Protocol::Tcp;

	fn open(parts: Parts) -> Self {
		let Parts {
			shared,
			producer: track,
			subscriber,
			peer,
			session,
		} = parts;
		let (producer, ours) = match track.create_group(group::Info { sequence: 0 }) {
			Ok(group) => {
				let handle = group.handle();
				(group, Some(handle))
			}
			// The track already ended, such as by the endpoint closing: hand out a half
			// that reports it.
			Err(err) => {
				let group = group::Producer::new(group::Info { sequence: 0 }, track::Info::default(), Default::default());
				let _ = group.handle().abort(err);
				(group, None)
			}
		};
		// The application reads a copy of the peer's half that the pump fills, so the
		// session can end it: the peer's own copy is the session's, not this side's.
		let local = group::Producer::new(group::Info { sequence: 0 }, track::Info::default(), Default::default());
		let consumer = local.consume();
		let pump = Pump {
			shared: shared.clone(),
			track: Some(track),
			ours,
			subscriber: Some(subscriber),
			remote: None,
			local_handle: local.handle(),
			local: Some(local),
			pending: None,
		};
		shared.tasks.push(kio::Pending::new(pump));
		Self {
			producer,
			consumer,
			peer,
			session,
		}
	}
}

/// A `track` session: each half is a whole track, and each message a group.
///
/// [`Self::consumer`] starts subscribed reliably and in order: oldest first, with no
/// delay budget, so every group arrives. Update its subscription, such as with a finite
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

impl Stream for TrackStream {}

impl sealed::Open for TrackStream {
	const PROTOCOL: Protocol = Protocol::Track;

	fn open(parts: Parts) -> Self {
		Self {
			producer: parts.producer,
			consumer: parts.subscriber,
			peer: parts.peer,
			session: parts.session,
		}
	}
}

/// Dials from peers in one protocol, waiting to be accepted.
#[derive(Default)]
struct Queue {
	/// Whether the protocol is accepted: from its first accept on.
	listening: bool,
	dials: VecDeque<track::Request>,
	/// The accepts waiting for a dial.
	waiting: kio::WaiterList,
}

/// What the endpoint's pollers share.
struct State {
	/// The dials peers send, from the first accept on; `None` until then, so the endpoint
	/// answers a request for a session it does not hold with [`Error::NotFound`] at once.
	requests: Option<super::broadcast::Requests>,
	tcp: Queue,
	track: Queue,
	/// This side's half of every session handed out, aborted when the endpoint closes.
	streams: Vec<track::TrackWeak>,
	/// Why the endpoint stopped, once it did.
	end: Option<Error>,
}

impl State {
	fn queue(&mut self, protocol: Protocol) -> &mut Queue {
		match protocol {
			Protocol::Tcp => &mut self.tcp,
			Protocol::Track => &mut self.track,
		}
	}

	/// Keep `producer`'s track, so the endpoint closing ends it.
	fn hold(&mut self, producer: &track::Producer) {
		self.streams.retain(|stream| !stream.is_closed());
		self.streams.push(producer.weak());
	}
}

impl Shared {
	/// Queue every dial on hand and take the next in `protocol`, registering `waiter` for
	/// more, and fail with the endpoint's end once it stopped.
	///
	/// A dial of this endpoint's reserves its own track, so a request reaching the handler
	/// under this endpoint's id names a session it never dialed, or one it gave up, such
	/// as an accept of a dial forged in this endpoint's name: it is reset with
	/// [`Error::NotFound`], which fails that accept. A request in a protocol nobody accepts
	/// is reset with [`Error::Unsupported`]. Resets and wakes go out after the lock is
	/// released.
	fn poll_dial(&self, protocol: Protocol, waiter: &kio::Waiter) -> Poll<Result<track::Request, Error>> {
		let mut resets = Vec::new();
		let mut wakes = Vec::new();
		let mut state = self.state.lock();
		state.queue(protocol).listening = true;
		while state.end.is_none() {
			let requests = state.requests.get_or_insert_with(|| self.publication.requests());
			match requests.poll_requested_track(waiter) {
				Poll::Pending => break,
				Poll::Ready(Ok(request)) => {
					// `None` for a name naming no dial: outside the scheme, or this
					// endpoint's own.
					let target = match parse(request.name()) {
						Some(name) if name.dialer != &*self.id => Some(Protocol::parse(name.protocol)),
						_ => None,
					};
					match target {
						Some(Some(protocol)) if state.queue(protocol).listening => {
							let queue = state.queue(protocol);
							queue.dials.push_back(request);
							wakes.push(queue.waiting.take());
						}
						Some(_) => resets.push((request, Error::Unsupported)),
						None => resets.push((request, Error::NotFound)),
					}
				}
				Poll::Ready(Err(err)) => state.end = Some(err),
			}
		}
		let next = match &state.end {
			Some(err) => Poll::Ready(Err(err.clone())),
			None => {
				let queue = state.queue(protocol);
				match queue.dials.pop_front() {
					Some(request) => Poll::Ready(Ok(request)),
					None => {
						queue.waiting.register(waiter);
						Poll::Pending
					}
				}
			}
		};
		drop(state);
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
		let mut guard = self.state.lock();
		let state = &mut *guard;
		state.end.get_or_insert(Error::Closed);
		let requests = state.requests.take();
		let mut queued = Vec::new();
		let mut wakes = Vec::new();
		for queue in [&mut state.tcp, &mut state.track] {
			queued.extend(queue.dials.drain(..));
			wakes.push(queue.waiting.take());
		}
		let streams = std::mem::take(&mut state.streams);
		drop(guard);

		self.publication.close();
		// Reset outside the lock.
		for stream in streams {
			stream.abort(Error::Cancel);
		}
		for request in queued {
			request.reject(Error::Cancel);
		}
		for mut wake in wakes {
			wake.wake();
		}
		drop(requests);
	}
}

/// A socket endpoint: announces `<prefix>/.socket/<id>`, dials other endpoints, and
/// accepts their dials.
///
/// It accepts dials in a protocol from that protocol's first accept on, like a TCP
/// listener: before that, a dial in it is reset with [`Error::Unsupported`], and after it,
/// dials queue until accepted. Until the endpoint's first accept in any protocol, a dial
/// to it is reset with [`Error::NotFound`], as for any track nobody serves.
///
/// Dropping it, or [`Self::close`], retracts the broadcast, resets the dials waiting to be
/// accepted, and resets every session it handed out: a [`TcpStream`] entirely, and this
/// side's half of a [`TrackStream`]. The peer's half of a [`TrackStream`] is read through
/// its [`TrackStream::consumer`], which the application owns: drop the streams too, which
/// releases the subscriptions.
///
/// `tcp` sessions run a task on the origin's driver, which must keep running.
pub struct Endpoint {
	shared: Arc<Shared>,
}

impl Endpoint {
	/// Announce the endpoint `id` under `prefix` on `origin`, which also reaches the peers.
	///
	/// `id` names the endpoint: a well-known name for a server, or a random one for a
	/// client. It must be one path segment. Fails like
	/// [`origin::Producer::create_broadcast`] when `origin` may not publish
	/// `<prefix>/.socket/<id>`.
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
				tasks: origin.tasks(),
				publication,
				state: kio::Lock::new(State {
					requests: None,
					tcp: Queue::default(),
					track: Queue::default(),
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

	/// Dial the endpoint `peer` in a new `tcp` session, resolving once the peer accepts it.
	///
	/// Waits for as long as the peer takes to accept: drop the returned future to give up,
	/// which resets the session. That includes waiting for the peer's endpoint to be
	/// announced. Fails with the peer's reset when it rejects the dial, or does not accept
	/// `tcp` sessions, and with [`Error::Unsupported`] for a dial to this endpoint itself.
	pub fn dial_tcp(&self, peer: &str) -> Result<kio::Pending<Dialing<TcpStream>>, Error> {
		self.dial(peer, None)
	}

	/// Dial the endpoint `peer` in a new `track` session; see [`Self::dial_tcp`].
	///
	/// `info` describes this side's half.
	pub fn dial_track(
		&self,
		peer: &str,
		info: impl Into<Option<track::Info>>,
	) -> Result<kio::Pending<Dialing<TrackStream>>, Error> {
		self.dial(peer, info.into())
	}

	fn dial<S: Stream>(&self, peer: &str, info: Option<track::Info>) -> Result<kio::Pending<Dialing<S>>, Error> {
		let shared = &self.shared;
		let path = endpoint_path(&shared.prefix, peer)?;
		if peer == &*shared.id {
			return Err(Error::Unsupported);
		}
		if let Some(err) = &shared.state.lock().end {
			return Err(err.clone());
		}
		let session = session_id();
		let name = track_name(S::PROTOCOL.as_str(), &shared.id, &session);
		// This side's half, waiting for the peer to ask for it, which only the peer can:
		// nobody else knows the session.
		let answer = shared.publication.reserve_unlisted_track(name.clone())?;
		// Followed rather than requested, so a peer not announced yet is waited for.
		let resolving = Resolving::Follow(Box::new(shared.origin.follow(path, None)?));
		Ok(kio::Pending::new(Dialing {
			shared: shared.clone(),
			peer: peer.into(),
			session,
			name,
			info,
			demand: answer.demand(),
			answer: Some(Answer::Reserved(answer)),
			step: Step::Resolving(resolving),
			stream: PhantomData,
		}))
	}

	fn poll_accept<S: Stream>(&self, waiter: &kio::Waiter) -> Poll<Result<Incoming<S>, Error>> {
		let shared = &self.shared;
		let request = ready!(shared.poll_dial(S::PROTOCOL, waiter))?;
		let name = parse(request.name()).expect("sorted as a dial");
		Poll::Ready(Ok(Incoming {
			shared: shared.clone(),
			peer: name.dialer.into(),
			session: name.session.into(),
			request: Some(request),
			stream: PhantomData,
		}))
	}

	/// Poll for the next `tcp` dial from a peer; fails with [`Error::Closed`] once the
	/// endpoint closed.
	///
	/// The first call starts accepting `tcp` dials. They queue until taken, without limit,
	/// each holding the dialer's request open, so keep accepting, and accept or reject
	/// each one.
	pub fn poll_accept_tcp(&self, waiter: &kio::Waiter) -> Poll<Result<Incoming<TcpStream>, Error>> {
		self.poll_accept(waiter)
	}

	/// Wait for the next `tcp` dial from a peer; see [`Self::poll_accept_tcp`].
	#[cfg(feature = "async")]
	pub async fn accept_tcp(&self) -> Result<Incoming<TcpStream>, Error> {
		kio::wait(|waiter| self.poll_accept_tcp(waiter)).await
	}

	/// Poll for the next `track` dial from a peer; see [`Self::poll_accept_tcp`].
	pub fn poll_accept_track(&self, waiter: &kio::Waiter) -> Poll<Result<Incoming<TrackStream>, Error>> {
		self.poll_accept(waiter)
	}

	/// Wait for the next `track` dial from a peer; see [`Self::poll_accept_tcp`].
	#[cfg(feature = "async")]
	pub async fn accept_track(&self) -> Result<Incoming<TrackStream>, Error> {
		kio::wait(|waiter| self.poll_accept_track(waiter)).await
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

/// A dial from a peer, from [`Endpoint::accept_tcp`] or [`Endpoint::accept_track`]: accept
/// or reject it.
///
/// Dropping it rejects it with [`Error::Cancel`].
pub struct Incoming<S: Stream> {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	/// The dialer's request for this side's half; taken by an accept or a reject.
	request: Option<track::Request>,
	stream: PhantomData<fn() -> S>,
}

impl<S: Stream> Incoming<S> {
	/// The id the dialer claims. An accept checks it: the dial connects only if that
	/// endpoint really dialed this session.
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

	fn accept_with(mut self, info: Option<track::Info>) -> kio::Pending<Accepting<S>> {
		let shared = self.shared.clone();
		let (peer, session) = (self.peer.clone(), self.session.clone());
		let request = self.request.take();
		// Requested, so a dial claiming an endpoint nobody announces fails at once: the
		// dialer announced its endpoint before it dialed.
		let resolving = endpoint_path(&shared.prefix, &peer)
			.map(|path| Resolving::Request(Box::new(shared.origin.request(path, None).into_inner())));
		let step = match resolving {
			Ok(resolving) => Step::Resolving(resolving),
			Err(err) => Step::Failed(err),
		};
		kio::Pending::new(Accepting {
			shared,
			name: track_name(S::PROTOCOL.as_str(), &peer, &session),
			peer,
			session,
			info,
			request,
			step,
			stream: PhantomData,
		})
	}
}

impl Incoming<TcpStream> {
	/// Accept the dial, resolving once the dialer answers, which proves it dialed.
	///
	/// Nothing is written before the dialer answers.
	pub fn accept(self) -> kio::Pending<Accepting<TcpStream>> {
		self.accept_with(None)
	}
}

impl Incoming<TrackStream> {
	/// Accept the dial, resolving once the dialer answers, which proves it dialed.
	///
	/// `info` describes this side's half. Nothing is written before the dialer answers.
	pub fn accept(self, info: impl Into<Option<track::Info>>) -> kio::Pending<Accepting<TrackStream>> {
		self.accept_with(info.into())
	}
}

impl<S: Stream> Drop for Incoming<S> {
	fn drop(&mut self) {
		if let Some(request) = self.request.take() {
			request.reject(Error::Cancel);
		}
	}
}

/// Resolving the peer's endpoint broadcast.
enum Resolving {
	/// Failing once the endpoint is not announced.
	Request(Box<origin::Requesting>),
	/// Waiting for the endpoint to be announced.
	Follow(Box<origin::Following>),
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
				Self::Resolving(resolving) => {
					let broadcast = ready!(resolving.poll(waiter))?;
					let subscribing = broadcast.track(track)?.subscribe(reliable()).into_inner();
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
fn open<S: Stream>(
	shared: &Arc<Shared>,
	producer: track::Producer,
	step: &mut Step,
	peer: &Arc<str>,
	session: &Arc<str>,
) -> Result<S, Error> {
	let mut state = shared.state.lock();
	if let Some(err) = state.end.clone() {
		drop(state);
		let _ = producer.abort(err.clone());
		return Err(err);
	}
	state.hold(&producer);
	drop(state);
	Ok(S::open(Parts {
		shared: shared.clone(),
		producer,
		subscriber: step.take_subscriber(),
		peer: peer.clone(),
		session: session.clone(),
	}))
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

/// A dial, from [`Endpoint::dial_tcp`] or [`Endpoint::dial_track`]: resolves once the peer
/// accepts.
///
/// Dropping it gives up, resetting the session.
pub struct Dialing<S: Stream> {
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
	stream: PhantomData<fn() -> S>,
}

impl<S: Stream> Dialing<S> {
	/// The session's id.
	pub fn session(&self) -> &str {
		&self.session
	}

	/// Poll for the peer to accept; see [`Endpoint::dial_tcp`].
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<S, Error>> {
		let result = self.poll_inner(waiter);
		if let Poll::Ready(Err(err)) = &result {
			if let Some(answer) = self.answer.take() {
				answer.reset(err.clone());
			}
			self.step = Step::Done;
		}
		result
	}

	fn poll_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<S, Error>> {
		self.shared.poll_end(waiter)?;

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

impl<S: Stream> kio::Task for Dialing<S> {
	type Output = Result<S, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

impl<S: Stream> Drop for Dialing<S> {
	fn drop(&mut self) {
		// Given up: reset the session for the peer.
		if let Some(answer) = self.answer.take() {
			answer.reset(Error::Cancel);
		}
	}
}

/// An accept, from an [`Incoming`] dial: resolves once the dialer answers.
pub struct Accepting<S: Stream> {
	shared: Arc<Shared>,
	peer: Arc<str>,
	session: Arc<str>,
	/// The session's track name.
	name: String,
	info: Option<track::Info>,
	/// The dialer's request for this side's half, served once the dialer answers.
	request: Option<track::Request>,
	step: Step,
	stream: PhantomData<fn() -> S>,
}

impl<S: Stream> Accepting<S> {
	/// Poll for the dialer to answer, which proves it dialed.
	///
	/// Fails with the dialer's reset when it never dialed this session, such as a dial
	/// forged in its name, and resets the dialer's request with the same error.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<S, Error>> {
		let result = self.poll_inner(waiter);
		if let Poll::Ready(Err(err)) = &result {
			self.step = Step::Done;
			if let Some(request) = self.request.take() {
				request.reject(err.clone());
			}
		}
		result
	}

	fn poll_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<S, Error>> {
		self.shared.poll_end(waiter)?;
		ready!(self.step.poll(waiter, &self.name))?;

		// Accepted outside the lock, since it wakes the dialer.
		let request = self.request.take().ok_or(Error::Closed)?;
		let producer = request.accept(self.info.take());
		Poll::Ready(open(&self.shared, producer, &mut self.step, &self.peer, &self.session))
	}
}

impl<S: Stream> kio::Task for Accepting<S> {
	type Output = Result<S, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

impl<S: Stream> Drop for Accepting<S> {
	fn drop(&mut self) {
		// Given up: reset the dialer's request.
		if let Some(request) = self.request.take() {
			request.reject(Error::Cancel);
		}
	}
}

/// A `tcp` session's task on the origin's driver.
///
/// It copies the peer's half into the group the application reads, a frame at a time and
/// paced by that reader, so backpressure reaches the peer. It ends this side's half the
/// way the application ended its group, and resets the session on a second group from the
/// peer, the application dropping its reader, or the endpoint closing.
struct Pump {
	shared: Arc<Shared>,
	/// This side's half, held so the application can write only its one group.
	track: Option<track::Producer>,
	/// This side's group, watched for how the application ends it.
	ours: Option<group::Handle>,
	/// The peer's half, watched for its one group, and for a second.
	subscriber: Option<track::Subscriber>,
	/// The peer's group, once it arrived.
	remote: Option<group::Consumer>,
	/// The application's copy of the peer's group, until it ends.
	local: Option<group::Producer>,
	local_handle: group::Handle,
	/// A frame read from the peer, waiting for the application's reader to make room.
	pending: Option<frame::Frame>,
}

impl Pump {
	/// Reset everything still open with `err`.
	fn reset(&mut self, err: Error) {
		if let Some(local) = self.local.take() {
			let _ = local.abort(err.clone());
		}
		if let Some(track) = self.track.take() {
			let _ = track.abort(err);
		}
		self.ours = None;
		self.subscriber = None;
		self.remote = None;
		self.pending = None;
	}

	fn poll_session(&mut self, waiter: &kio::Waiter) -> Result<(), Error> {
		self.shared.poll_end(waiter)?;

		// This side's half ends the way the application ended its group.
		if let Some(ours) = &self.ours
			&& let Poll::Ready(outcome) = ours.poll_closed(waiter)
		{
			self.ours = None;
			match outcome {
				Ok(()) => {
					if let Some(track) = &self.track {
						let _ = track.finish();
					}
				}
				Err(err) => {
					if let Some(track) = self.track.take() {
						let _ = track.abort(err);
					}
				}
			}
		}

		if self.local.is_none() {
			return Ok(());
		}
		// Nobody reads what the peer writes.
		if self.local_handle.poll_unwanted(waiter).is_ready() {
			return Err(Error::Cancel);
		}

		// The peer's half holds one group: a second is a protocol error.
		if let Some(subscriber) = &mut self.subscriber {
			loop {
				match subscriber.poll_recv_group(waiter) {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(group))) => {
						if self.remote.is_some() || group.sequence != 0 {
							tracing::debug!(sequence = group.sequence, "a second group on a tcp session");
							return Err(Error::ProtocolViolation);
						}
						self.remote = Some(group);
					}
					Poll::Ready(Ok(None)) => {
						self.subscriber = None;
						break;
					}
					Poll::Ready(Err(err)) => {
						// Ended short of its group: the peer's reset.
						if self.remote.is_none() {
							return Err(err);
						}
						self.subscriber = None;
						break;
					}
				}
			}
		}

		let Some(remote) = &mut self.remote else {
			// Finished without a group: an empty half.
			if self.subscriber.is_none()
				&& let Some(mut local) = self.local.take()
			{
				let _ = local.finish();
			}
			return Ok(());
		};
		let local = self.local.as_mut().expect("checked above");
		loop {
			if let Some(frame) = &self.pending {
				let info = frame::Info {
					size: frame.payload.len() as u64,
					timestamp: None,
				};
				if local.poll_ready(waiter, &info).is_pending() {
					return Ok(());
				}
				let frame = self.pending.take().expect("checked above");
				local.write_frame(None, frame.payload)?;
			}
			match remote.poll_read_frame(waiter) {
				Poll::Pending => return Ok(()),
				Poll::Ready(Ok(Some(frame))) => self.pending = Some(frame),
				Poll::Ready(Ok(None)) => {
					local.finish()?;
					self.local = None;
					self.remote = None;
					return Ok(());
				}
				Poll::Ready(Err(err)) => return Err(err),
			}
		}
	}
}

impl kio::Task for Pump {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = self.poll_session(waiter) {
			self.reset(err);
		}
		// Done once both halves ended; dropping the track then leaves its end in place.
		if self.ours.is_none() && self.local.is_none() {
			return Poll::Ready(());
		}
		Poll::Pending
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn track_names_round_trip() {
		let name = track_name("tcp", "alice", "0123abcd");
		assert_eq!(name, "tcp/alice/0123abcd");
		assert_eq!(
			parse(&name),
			Some(Name {
				protocol: "tcp",
				dialer: "alice",
				session: "0123abcd"
			})
		);
	}

	#[test]
	fn foreign_track_names_are_skipped() {
		for name in [
			"command",
			"alice/s",
			"tcp/alice/",
			"tcp//s",
			"/alice/s",
			"tcp/alice/s/0",
			"tcp/a\\b/s",
			"tcp/alice/s\\",
		] {
			assert_eq!(parse(name), None, "{name}");
		}
	}

	#[test]
	fn an_endpoint_path_holds_one_segment() {
		let prefix = Path::from_static("room");
		assert_eq!(endpoint_path(&prefix, "alice").unwrap().as_str(), "room/.socket/alice");
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
