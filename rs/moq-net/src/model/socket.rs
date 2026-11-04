//! Point-to-point sockets over an origin: a [`Listener`] accepts the [`Socket`]s that
//! [`dial`]ers open to its path.
//!
//! A socket is a convention over announcements and broadcasts, with no wire of its own.
//! A listener at `L` keeps `L` for whatever it shares with everyone. A dialer publishes
//! its half at `L/.dial/<id>`, and the listener answers with its half at `L/.accept/<id>`
//! once the application accepts. The answer carries the dial's [`Instance`], so a
//! dialer that dials again with the same id gets a new socket rather than an old answer.
//!
//! Inside a socket either side opens and accepts streams the way QUIC does, and like
//! QUIC's they carry no name: a stream is told apart by who opened it and its number. A
//! uni stream is one track in the opener's broadcast, and a bi stream is the same track
//! name in both. Each open takes the next number of its opener, so a track never carries
//! two streams. The peer learns of new streams from the broadcast's track listing, which
//! needs moq-lite-07 on every hop. Channels both sides know in advance are ordinary
//! fixed-name tracks on [`Socket::local`] and [`Socket::remote`]. See
//! `doc/concept/socket.md` for the convention.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Poll, ready};

use hashlink::{LinkedHashMap, LinkedHashSet};

use crate::{Error, Instance, Path, Pattern, Patterns};

use super::broadcast::{self, TracksEvent};
use super::origin::{self, Route};
use super::{announce, track};

/// The segment beneath a listener's path that dialers publish under.
const DIAL: &str = ".dial";
/// The segment beneath a listener's path that the listener answers under.
const ACCEPT: &str = ".accept";

/// Which end of a socket a side is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
	Dialer,
	Listener,
}

impl Role {
	/// The tag naming the streams this role opens.
	fn tag(self) -> &'static str {
		match self {
			Self::Dialer => "dial",
			Self::Listener => "accept",
		}
	}
}

/// Whether a stream carries data one way or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	Uni,
	Bi,
}

impl Kind {
	fn tag(self) -> &'static str {
		match self {
			Self::Uni => "uni",
			Self::Bi => "bi",
		}
	}
}

/// The track name of the opener's `number`th stream, a `kind` one:
/// `.<opener>/<kind>/<number>`.
fn track_name(opener: Role, kind: Kind, number: u64) -> String {
	format!(".{}/{}/{number}", opener.tag(), kind.tag())
}

/// Split a track name into its opener and kind, or `None` for a track outside the
/// scheme, such as one the application names itself.
fn parse(track: &str) -> Option<(Role, Kind)> {
	let (opener, rest) = track.strip_prefix('.')?.split_once('/')?;
	let (kind, number) = rest.split_once('/')?;
	// Decimal, as the opener writes it, so one stream has one name.
	if number.parse::<u64>().ok()?.to_string() != number {
		return None;
	}
	let opener = match opener {
		"dial" => Role::Dialer,
		"accept" => Role::Listener,
		_ => return None,
	};
	let kind = match kind {
		"uni" => Kind::Uni,
		"bi" => Kind::Bi,
		_ => return None,
	};
	Some((opener, kind))
}

/// `L/<side>/<id>`, refusing an id that is not exactly one path segment.
fn side(path: &Path, side: &'static str, id: &str) -> Result<Path, Error> {
	let segment = Path::new(id)?;
	if segment.parts().count() != 1 {
		return Err(crate::InvalidPattern::InvalidSegment(id.to_string()).into());
	}
	Ok(path.join(&Path::from_static(side))?.join(&segment)?)
}

/// Why a [`Socket`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Closed {
	/// The peer's broadcast ended: it closed the socket, or a session on the way dropped.
	#[error("the peer ended the socket")]
	Remote,
	/// This side's broadcast ended, through [`Socket::close`] or otherwise.
	#[error("the socket was closed locally")]
	Local,
	/// The dialer dialed again with the same id, which replaces this socket.
	#[error("a newer dial replaced the socket")]
	Replaced,
}

/// A bi stream: this side's track and the peer's, under one track name.
pub struct Bi {
	/// This side's track.
	pub producer: track::Producer,
	/// The peer's track.
	pub consumer: track::Consumer,
}

/// What the peer's track listing has reported and nobody has taken yet.
#[derive(Default)]
struct Listing {
	/// The peer's listing, opened by the first accept or bi open.
	tracks: Option<broadcast::Tracks>,
	/// Uni streams the peer opened, not yet accepted, in order, by track name. A stream
	/// leaves once the peer stops listing it, as the listing drops an add its reader
	/// never reached, so these hold no more than the peer lists.
	uni: LinkedHashSet<Arc<str>>,
	/// Bi streams the peer opened, with the opener's track info, not yet accepted.
	bi: LinkedHashMap<Arc<str>, track::Info>,
	/// This side reading the opener's half of each bi stream it accepted, by track name,
	/// until the opener's half leaves the listing or this side closes. The opener learns
	/// of the accept from this demand, not from the answer's listing: a listing drops a
	/// track that comes and goes before its reader reaches it, so an answer dropped at once
	/// would never be seen, and the application may not read the opener's half before it
	/// hears back. `None` while an accept is answering the stream, so a removal drained
	/// meanwhile, by any poller, also drops the read the accept is about to register.
	reading: HashMap<Arc<str>, Option<kio::Pending<track::Subscribing>>>,
	/// How the listing ended: `Ok` once the peer's broadcast ended, or why it failed.
	end: Option<Result<(), Error>>,
}

impl Listing {
	/// Sort every event on hand into the queues, registering `waiter` for the next.
	///
	/// Every poller drains the one listing, so each sorts what it reads for the others:
	/// they all park on the listing, which wakes each of them on a change.
	fn drain(&mut self, waiter: &kio::Waiter, role: Role, remote: &broadcast::Consumer) {
		if self.end.is_some() {
			return;
		}
		let tracks = match &mut self.tracks {
			Some(tracks) => tracks,
			None => match remote.tracks() {
				Ok(tracks) => self.tracks.insert(tracks),
				// The peer's broadcast already ended.
				Err(Error::Unroutable) => {
					self.end = Some(Ok(()));
					return;
				}
				Err(err) => {
					self.end = Some(Err(err));
					return;
				}
			},
		};
		loop {
			let event = match tracks.poll_next(waiter) {
				Poll::Pending => return,
				Poll::Ready(Ok(Some(event))) => event,
				Poll::Ready(Ok(None)) => {
					self.end = Some(Ok(()));
					self.reading.clear();
					return;
				}
				Poll::Ready(Err(err)) => {
					self.end = Some(Err(err));
					self.reading.clear();
					return;
				}
			};
			let (name, info) = match event {
				TracksEvent::Added { name, info } => (name, Some(info)),
				TracksEvent::Removed { name } | TracksEvent::Finished { name } => (name, None),
				TracksEvent::Synced => continue,
			};
			// A track outside the scheme is the application's own.
			let Some((opener, kind)) = parse(&name) else {
				continue;
			};
			match (opener == role, kind, info) {
				(false, Kind::Uni, Some(_)) => {
					self.uni.replace(name);
				}
				(false, Kind::Uni, None) => {
					self.uni.remove(&name);
				}
				(false, Kind::Bi, Some(info)) => {
					self.bi.replace(name, info);
				}
				(false, Kind::Bi, None) => {
					self.bi.remove(&name);
					// The opener dropped its half, so it resolved or gave up.
					self.reading.remove(&name);
				}
				// An open learns of its answer from the peer reading it, not the listing.
				(true, Kind::Bi, _) => {}
				// This side's uni streams live only in its own broadcast.
				(true, Kind::Uni, _) => {}
			}
		}
	}

	/// The listing's end as what an accept returns: `None` once the peer is gone.
	fn ended<T>(&self) -> Option<Result<Option<T>, Error>> {
		match self.end.as_ref()? {
			Ok(()) => Some(Ok(None)),
			Err(err) => Some(Err(err.clone())),
		}
	}
}

/// What a socket shares with its pending bi opens and its listener.
struct Inner {
	role: Role,
	id: Arc<str>,
	/// This side's broadcast.
	local: origin::Publication,
	/// The peer's broadcast.
	remote: broadcast::Consumer,
	listing: kio::Lock<Listing>,
	/// Why the listener ended the socket, set before it closes [`Self::local`].
	closed: kio::Lock<Option<Closed>>,
	/// The number of this side's next stream.
	next: AtomicU64,
}

impl Inner {
	/// End the socket for `reason`, retracting this side's broadcast.
	fn close(&self, reason: Closed) {
		self.closed.lock().get_or_insert(reason);
		self.retract();
	}

	/// Retract this side's broadcast and stop reading the opener's half of every bi stream
	/// it accepted. Closed first, so an accept registering a read after the release sees
	/// the close and drops it.
	fn retract(&self) {
		self.local.close();
		// Dropped outside the lock.
		let reading = std::mem::take(&mut self.listing.lock().reading);
		drop(reading);
	}

	/// Whether this side's broadcast ended, registering `waiter` for when it does.
	fn poll_local_closed(&self, waiter: &kio::Waiter) -> bool {
		self.local.poll_closed(waiter).is_ready()
	}

	/// The track name of this side's next `kind` stream.
	fn open(&self, kind: Kind) -> String {
		track_name(self.role, kind, self.next.fetch_add(1, Ordering::Relaxed))
	}

	/// Create a track in this side's broadcast, refusing with [`Error::Closed`] once it
	/// ended: a track in an ended broadcast reaches nobody. Checked after the create, so a
	/// close racing it is either seen here or comes after the stream opened.
	fn create_track(&self, name: String, info: impl Into<Option<track::Info>>) -> Result<track::Producer, Error> {
		let producer = self.local.create_track(name, info)?;
		if self.poll_local_closed(&kio::Waiter::noop()) {
			return Err(Error::Closed);
		}
		Ok(producer)
	}
}

/// One end of a connection between a dialer and a listener.
///
/// Holds this side's broadcast and the peer's. Drop it once it ends: dropping retracts
/// this side's broadcast, which ends the socket for the peer too.
pub struct Socket {
	inner: Arc<Inner>,
}

impl Socket {
	fn new(role: Role, id: Arc<str>, local: origin::Publication, remote: broadcast::Consumer) -> Self {
		Self {
			inner: Arc::new(Inner {
				role,
				id,
				local,
				remote,
				listing: kio::Lock::new(Listing::default()),
				closed: kio::Lock::new(None),
				next: AtomicU64::new(0),
			}),
		}
	}

	/// The id the dialer chose.
	pub fn id(&self) -> &str {
		&self.inner.id
	}

	/// This side's broadcast, for well-known channels: tracks with fixed names that both
	/// sides agree on, beside the streams.
	pub fn local(&self) -> &broadcast::Producer {
		&self.inner.local
	}

	/// The peer's broadcast, for well-known channels: tracks with fixed names that both
	/// sides agree on, beside the streams.
	pub fn remote(&self) -> &broadcast::Consumer {
		&self.inner.remote
	}

	/// Open a uni stream: a new track only this side writes, which the peer takes with
	/// [`Self::accept_uni`].
	///
	/// `info` describes the track. Fails with [`Error::Closed`] once this side closed.
	pub fn open_uni(&self, info: impl Into<Option<track::Info>>) -> Result<track::Producer, Error> {
		self.inner.create_track(self.inner.open(Kind::Uni), info)
	}

	/// Open a bi stream, resolving once the peer accepts it with [`Self::accept_bi`].
	///
	/// `info` describes this side's track, and the peer's answer mirrors it. Fails with
	/// [`Error::Closed`] once this side closed.
	pub fn open_bi(&self, info: impl Into<Option<track::Info>>) -> Result<kio::Pending<OpenBi>, Error> {
		let inner = &self.inner;
		let track: Arc<str> = inner.open(Kind::Bi).into();
		let producer = inner.create_track(track.to_string(), info)?;
		Ok(kio::Pending::new(OpenBi {
			inner: inner.clone(),
			track,
			demand: producer.demand(),
			producer: Some(producer),
		}))
	}

	/// Poll for the next uni stream the peer opened; `None` once the peer's broadcast ended,
	/// and [`Error::Closed`] once this side closed.
	///
	/// A stream is handed out only while the peer still lists it: one whose track the peer
	/// dropped first, finished or not, is skipped.
	pub fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Option<track::Consumer>, Error>> {
		let inner = &self.inner;
		loop {
			if inner.poll_local_closed(waiter) {
				return Poll::Ready(Err(Error::Closed));
			}
			let mut listing = inner.listing.lock();
			listing.drain(waiter, inner.role, &inner.remote);
			let Some(track) = listing.uni.pop_front() else {
				return match listing.ended() {
					Some(end) => Poll::Ready(end),
					None => Poll::Pending,
				};
			};
			drop(listing);
			return match inner.remote.track(&track) {
				Ok(consumer) => Poll::Ready(Ok(Some(consumer))),
				// The peer's broadcast ended.
				Err(Error::Unroutable) => Poll::Ready(Ok(None)),
				// The peer dropped the track since the listing read it.
				Err(Error::NotFound) => continue,
				Err(err) => Poll::Ready(Err(err)),
			};
		}
	}

	/// Wait for the next uni stream the peer opened; see [`Self::poll_accept_uni`].
	#[cfg(feature = "async")]
	pub async fn accept_uni(&self) -> Result<Option<track::Consumer>, Error> {
		kio::wait(|waiter| self.poll_accept_uni(waiter)).await
	}

	/// Poll for the next bi stream the peer opened, answering it with a track that mirrors
	/// the opener's info; `None` once the peer's broadcast ended, and [`Error::Closed`]
	/// once this side closed. Skips streams as [`Self::poll_accept_uni`] does.
	///
	/// The socket reads the opener's half until the opener drops it or this side closes,
	/// which is how the opener learns of the accept. As with any stream, the answer reaches the opener only
	/// while this side holds it: finishing it at once is fine, dropping it before the
	/// opener reads it is not.
	pub fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<Option<Bi>, Error>> {
		let inner = &self.inner;
		loop {
			if inner.poll_local_closed(waiter) {
				return Poll::Ready(Err(Error::Closed));
			}
			let mut listing = inner.listing.lock();
			listing.drain(waiter, inner.role, &inner.remote);
			let Some((track, info)) = listing.bi.pop_front() else {
				return match listing.ended() {
					Some(end) => Poll::Ready(end),
					None => Poll::Pending,
				};
			};
			// Already answered: the peer listed it again, which it does only by opening a
			// number twice.
			if listing.reading.contains_key(&track) {
				continue;
			}
			listing.reading.insert(track.clone(), None);
			drop(listing);

			let answered = self.answer_bi(&track, info);
			let mut listing = inner.listing.lock();
			let (bi, read) = match answered {
				Ok(Some(answered)) => answered,
				Ok(None) => {
					listing.reading.remove(&track);
					continue;
				}
				Err(err) => {
					listing.reading.remove(&track);
					return match err {
						// The peer's broadcast ended.
						Error::Unroutable => Poll::Ready(Ok(None)),
						err => Poll::Ready(Err(err)),
					};
				}
			};
			// Under the lock, so a close or a removal of the stream either comes after this
			// and drops the read, or came first and is seen here.
			let closed = inner.poll_local_closed(&kio::Waiter::noop());
			if !closed && let Some(slot) = listing.reading.get_mut(&track) {
				// How the opener learns of the accept.
				*slot = Some(read);
				return Poll::Ready(Ok(Some(bi)));
			}
			// Dropped outside the lock.
			let stale = listing.reading.remove(&track);
			drop(listing);
			drop((stale, read, bi));
			if closed {
				return Poll::Ready(Err(Error::Closed));
			}
			// The opener dropped its half while this answered it.
		}
	}

	/// Answer the peer's bi stream `track` with a track mirroring the opener's `info`, and
	/// start reading the opener's half. `None` for a stream to skip.
	fn answer_bi(
		&self,
		track: &str,
		info: track::Info,
	) -> Result<Option<(Bi, kio::Pending<track::Subscribing>)>, Error> {
		let inner = &self.inner;
		let consumer = match inner.remote.track(track) {
			Ok(consumer) => consumer,
			// The peer dropped the track since the listing read it.
			Err(Error::NotFound) => return Ok(None),
			Err(err) => return Err(err),
		};
		// The opener's track info, minus how its copy reached this hop.
		let info = track::Info {
			properties: None,
			unchecked: false,
			..info
		};
		// Answered under the opener's track name.
		let producer = match inner.create_track(track.to_string(), info) {
			Ok(producer) => producer,
			// This side still holds an answer under the name, which the peer listed again
			// only by opening a number twice.
			Err(Error::Duplicate) => return Ok(None),
			Err(err) => return Err(err),
		};
		let read = consumer.subscribe(None);
		Ok(Some((Bi { producer, consumer }, read)))
	}

	/// Wait for the next bi stream the peer opened; see [`Self::poll_accept_bi`].
	#[cfg(feature = "async")]
	pub async fn accept_bi(&self) -> Result<Option<Bi>, Error> {
		kio::wait(|waiter| self.poll_accept_bi(waiter)).await
	}

	/// Poll for the socket to end, and why.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Closed> {
		let inner = &self.inner;
		if let Some(reason) = *inner.closed.lock() {
			return Poll::Ready(reason);
		}
		if inner.local.poll_closed(waiter).is_ready() {
			// The listener may have ended it between the check above and the close.
			return Poll::Ready(inner.closed.lock().unwrap_or(Closed::Local));
		}
		ready!(inner.remote.poll_closed(waiter));
		Poll::Ready(Closed::Remote)
	}

	/// Wait for the socket to end, and why.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Closed {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// End the socket, retracting this side's broadcast.
	pub fn close(&self) {
		self.inner.close(Closed::Local);
	}
}

impl Drop for Socket {
	fn drop(&mut self) {
		// A pending bi open still holds the shared half, but not the socket.
		self.inner.retract();
	}
}

/// A bi stream being opened, from [`Socket::open_bi`]: resolves once the peer answers.
pub struct OpenBi {
	inner: Arc<Inner>,
	/// The stream's track name, which the answer shares.
	track: Arc<str>,
	/// This side's track, handed over once the peer answers.
	producer: Option<track::Producer>,
	/// Whether anyone reads this side's track, which the peer does from its accept on.
	demand: track::Demand,
}

impl OpenBi {
	/// Poll for the peer's answer, which it gives by reading this side's half. Any reader
	/// counts, so only the peer should be able to read this side's broadcast; see the
	/// authorization section of `doc/concept/socket.md`. Fails with
	/// [`Error::Unroutable`] once the peer's broadcast ends without one, and with
	/// [`Error::Closed`] once this side closed, even if the answer came.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bi, Error>> {
		let inner = &self.inner;
		// This side's track went with its broadcast, and a peer that keeps its own up
		// without answering would otherwise hold this open forever.
		if inner.poll_local_closed(waiter) {
			return Poll::Ready(Err(Error::Closed));
		}
		// Polled until it parks, so a later change wakes this.
		while let Poll::Ready(Ok(_)) = self.demand.poll_wanted_changed(waiter) {}
		if self.demand.wanted().is_some() {
			let consumer = inner.remote.track(&self.track)?;
			let producer = self.producer.take().ok_or(Error::Closed)?;
			return Poll::Ready(Ok(Bi { producer, consumer }));
		}
		let mut listing = inner.listing.lock();
		listing.drain(waiter, inner.role, &inner.remote);
		match &listing.end {
			Some(Ok(())) => Poll::Ready(Err(Error::Unroutable)),
			Some(Err(err)) => Poll::Ready(Err(err.clone())),
			None => Poll::Pending,
		}
	}
}

impl kio::Task for OpenBi {
	type Output = Result<Bi, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Dial the listener at `path` as `id`, from [`dial`]: resolves once the listener accepts.
pub struct Dialing {
	id: Arc<str>,
	/// This side's broadcast, announced for the listener to find.
	local: Option<origin::Publication>,
	/// The listener's answer, pinned to this dial's instance.
	answer: origin::Following,
}

impl Dialing {
	/// Poll for the listener's answer. Waits for as long as it takes: drop this to give up.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<Socket, Error>> {
		let remote = ready!(self.answer.poll_available(waiter))?;
		let local = self.local.take().ok_or(Error::Closed)?;
		Poll::Ready(Ok(Socket::new(Role::Dialer, self.id.clone(), local, remote)))
	}
}

impl kio::Task for Dialing {
	type Output = Result<Socket, Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Dial the listener at `path` as `id`, resolving once it accepts.
///
/// Publishes this side's broadcast at `path/.dial/<id>` with a fresh [`Instance`], and
/// waits for the listener's answer at `path/.accept/<id>` carrying that instance. There
/// is no timeout: drop the returned future to give up, which retracts the dial.
///
/// `id` must be one path segment; an id with a `/` fails with [`Error::InvalidPath`].
/// Fails like [`origin::Producer::create_broadcast`] when `origin` may not publish the
/// dial, and like [`origin::Consumer::follow`] when it may not read the answer.
pub fn dial(
	origin: &origin::Producer,
	path: impl TryInto<Path, Error: Into<Error>>,
	id: &str,
) -> Result<kio::Pending<Dialing>, Error> {
	let path = crate::path::convert(path)?;
	let instance = Instance::random();
	let local = origin.create_broadcast(side(&path, DIAL, id)?)?;
	let answer = origin.consume().follow(side(&path, ACCEPT, id)?, instance.clone())?;
	local.announce(Route::default().with_instance(instance, 0))?;
	Ok(kio::Pending::new(Dialing {
		id: id.into(),
		local: Some(local),
		answer,
	}))
}

/// A socket the listener handed out, kept so a newer dial or the dialer leaving ends it.
struct Live {
	instance: Instance,
	socket: Weak<Inner>,
}

/// A dial being accepted: its broadcast requested, pinned to its instance.
struct Accepting {
	id: Path,
	instance: Instance,
	request: origin::Requesting,
}

/// Accepts the sockets dialed to a path.
///
/// Dialers queue in the announcements until [`Self::accept`] takes them, without limit,
/// and a dialer's broadcast is requested only then: a listener that stops accepting
/// holds nothing for the dialers waiting on it. While it accepts, a dialer dialing again
/// with the same id ends the socket it replaces with [`Closed::Replaced`].
///
/// Run one listener per path. Two would both answer a dial under its instance, and the
/// dialer would follow only one of them, leaving the other's socket unread.
pub struct Listener {
	origin: origin::Producer,
	path: Path,
	/// The dialers' broadcasts, rooted at `path/.dial`.
	dials: origin::Consumer,
	announced: announce::Consumer,
	live: HashMap<Path, Live>,
	accepting: Option<Accepting>,
}

impl Listener {
	/// Listen at `path` on `origin`, which publishes the answers and reads the dials.
	///
	/// Fails with [`Error::Unauthorized`] when `origin` may not read `path/.dial`.
	pub fn new(origin: &origin::Producer, path: impl TryInto<Path, Error: Into<Error>>) -> Result<Self, Error> {
		let path = crate::path::convert(path)?;
		let ids = Patterns::from("*".parse::<Pattern>()?);
		// An id is any one segment, a dotted one included.
		let dials = origin
			.consume()
			.scope(path.join(&Path::from_static(DIAL))?, &ids)?
			.with_hidden(true);
		let announced = dials.announced();
		Ok(Self {
			origin: origin.clone(),
			path,
			dials,
			announced,
			live: HashMap::new(),
			accepting: None,
		})
	}

	/// The path this listener accepts dials at.
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Poll for the next dialer, requesting its broadcast and answering it.
	///
	/// Dialers are taken one at a time, in order. A session resolves a request for a
	/// broadcast its peer announced itself, without asking the peer, so a remote dialer
	/// cannot hold up the ones behind it; only an [`origin::Requests`] handler on this
	/// origin that holds a dial's request can. A dialer that leaves before its broadcast
	/// resolves is skipped. Fails with [`Error::Closed`] once the origin closes.
	pub fn poll_accept(&mut self, waiter: &kio::Waiter) -> Poll<Result<Socket, Error>> {
		loop {
			if let Some(accepting) = &mut self.accepting {
				let remote = ready!(accepting.request.poll_ok(waiter));
				let accepting = self.accepting.take().expect("accepting");
				match remote {
					Ok(remote) => match self.answer(accepting, remote) {
						Ok(Some(socket)) => return Poll::Ready(Ok(socket)),
						Ok(None) => {}
						Err(err) => return Poll::Ready(Err(err)),
					},
					Err(err) => tracing::debug!(id = %accepting.id, %err, "dialer left before it was accepted"),
				}
				continue;
			}

			let Some(event) = ready!(self.announced.poll_next(waiter)) else {
				return Poll::Ready(Err(Error::Closed));
			};
			let (announce, takeover) = match event {
				announce::Event::Start(announce) | announce::Event::Restart(announce) => (announce, false),
				announce::Event::Takeover { announce, .. } => (announce, true),
				// The same instance, delivered in place.
				announce::Event::Update(_) => continue,
				announce::Event::End(announce) => {
					// The dialer's routes are gone, and so is its broadcast; retract the
					// answer too, so a return of the same instance never meets it.
					if let Some(live) = self.live.remove(&announce.prefix)
						&& let Some(socket) = live.socket.upgrade()
					{
						socket.close(Closed::Remote);
					}
					continue;
				}
			};
			let Some(instance) = announce.route.instance else {
				continue;
			};
			if let Some(live) = self.live.get_mut(&announce.prefix) {
				match live.socket.upgrade() {
					Some(_) if live.instance == instance => continue,
					// A takeover continues the stream the live socket already reads.
					Some(_) if takeover => {
						live.instance = instance;
						continue;
					}
					Some(socket) => socket.close(Closed::Replaced),
					None => {}
				}
				self.live.remove(&announce.prefix);
			}
			let request = self.dials.request(&announce.prefix, instance.clone()).into_inner();
			self.accepting = Some(Accepting {
				id: announce.prefix,
				instance,
				request,
			});
		}
	}

	/// Wait for the next dialer; see [`Self::poll_accept`].
	#[cfg(feature = "async")]
	pub async fn accept(&mut self) -> Result<Socket, Error> {
		kio::wait(|waiter| self.poll_accept(waiter)).await
	}

	/// Publish the answer to a resolved dial, or `None` when this dialer's answer can't be
	/// published, such as an id this origin may not answer.
	fn answer(&mut self, accepting: Accepting, remote: broadcast::Consumer) -> Result<Option<Socket>, Error> {
		let Accepting { id, instance, .. } = accepting;
		let local = side(&self.path, ACCEPT, id.as_str())
			.and_then(|path| self.origin.create_broadcast(path))
			.and_then(|local| {
				local.announce(Route::default().with_instance(instance.clone(), 0))?;
				Ok(local)
			});
		let local = match local {
			Ok(local) => local,
			Err(Error::Closed) => return Err(Error::Closed),
			Err(err) => {
				tracing::warn!(%id, %err, "cannot answer the dialer");
				return Ok(None);
			}
		};
		let socket = Socket::new(Role::Listener, id.as_str().into(), local, remote);
		self.live.insert(
			id,
			Live {
				instance,
				socket: Arc::downgrade(&socket.inner),
			},
		);
		Ok(Some(socket))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn track_names_round_trip() {
		for opener in [Role::Dialer, Role::Listener] {
			for kind in [Kind::Uni, Kind::Bi] {
				let name = track_name(opener, kind, 7);
				assert_eq!(parse(&name), Some((opener, kind)));
			}
		}
		assert_eq!(track_name(Role::Dialer, Kind::Bi, 0), ".dial/bi/0");
		assert_eq!(track_name(Role::Listener, Kind::Uni, 12), ".accept/uni/12");
	}

	#[test]
	fn foreign_track_names_are_skipped() {
		for name in [
			"command",
			".dial",
			".dial/uni",
			".dial/uni/",
			".dial/other/0",
			".other/uni/0",
			"dial/uni/0",
			".dial/uni/x",
			".dial/uni/0/x",
			".dial/uni/07",
			".dial/uni/-1",
			".dial/uni/+1",
		] {
			assert_eq!(parse(name), None, "{name}");
		}
	}

	#[test]
	fn an_id_is_one_segment() {
		let path = Path::from_static("room");
		assert_eq!(side(&path, DIAL, "alice").unwrap().as_str(), "room/.dial/alice");
		assert!(side(&path, DIAL, "a/b").is_err());
		assert!(side(&path, DIAL, "").is_err());
	}
}
