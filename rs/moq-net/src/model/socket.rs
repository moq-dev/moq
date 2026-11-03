//! Point-to-point sockets over an origin: a [`Listener`] accepts the [`Socket`]s that
//! [`dial`]ers open to its path.
//!
//! A socket is a convention over announcements and broadcasts, with no wire of its own.
//! A listener at `L` keeps `L` for whatever it shares with everyone. A dialer publishes
//! its half at `L/.dial/<id>`, and the listener answers with its half at `L/.accept/<id>`
//! once the application accepts. The answer carries the dial's [`Instance`], so a
//! dialer that dials again with the same id gets a new socket rather than an old answer.
//!
//! Inside a socket either side opens tracks the way QUIC opens streams. A uni stream is
//! one track in the opener's broadcast, and a bi stream is the same track name in both.
//! The peer learns of new streams from the broadcast's track listing, which needs
//! moq-lite-07 on every hop. See `doc/concept/socket.md` for the convention.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::task::{Poll, ready};

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

	fn peer(self) -> Self {
		match self {
			Self::Dialer => Self::Listener,
			Self::Listener => Self::Dialer,
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

/// The track name of `name`, opened by `opener` as a `kind` stream: `.<opener>/<kind>/<name>`.
fn track_name(opener: Role, kind: Kind, name: &str) -> String {
	format!(".{}/{}/{name}", opener.tag(), kind.tag())
}

/// Split a track name into its opener, kind, and stream name, or `None` for a track
/// outside the scheme, such as one the application names itself.
fn parse(track: &str) -> Option<(Role, Kind, &str)> {
	let (opener, rest) = track.strip_prefix('.')?.split_once('/')?;
	let (kind, name) = rest.split_once('/')?;
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
	Some((opener, kind, name))
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

/// A uni stream the peer opened.
pub struct Uni {
	/// The name the peer opened it with.
	pub name: Arc<str>,
	/// The peer's track.
	pub consumer: track::Consumer,
}

/// A bi stream: this side's track and the peer's, under one name.
pub struct Bi {
	/// The name the opener gave it.
	pub name: Arc<str>,
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
	/// Uni streams the peer opened, not yet accepted.
	uni: VecDeque<Arc<str>>,
	/// Bi streams the peer opened, with the opener's track info, not yet accepted.
	bi: VecDeque<(Arc<str>, track::Info)>,
	/// This side's bi opens waiting for an answer, and whether the peer listed it.
	opening: HashMap<Arc<str>, bool>,
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
					return;
				}
				Poll::Ready(Err(err)) => {
					self.end = Some(Err(err));
					return;
				}
			};
			// Only additions open streams; a stream ends through its own track.
			let TracksEvent::Added { name, info } = event else {
				continue;
			};
			// A track outside the scheme is the application's own.
			let Some((opener, kind, stream)) = parse(&name) else {
				continue;
			};
			match (opener == role, kind) {
				(false, Kind::Uni) => self.uni.push_back(stream.into()),
				(false, Kind::Bi) => self.bi.push_back((stream.into(), info)),
				(true, Kind::Bi) => {
					if let Some(answered) = self.opening.get_mut(stream) {
						*answered = true;
					}
				}
				// This side's uni streams live only in its own broadcast.
				(true, Kind::Uni) => {}
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
}

impl Inner {
	/// End the socket for `reason`, retracting this side's broadcast.
	fn close(&self, reason: Closed) {
		self.closed.lock().get_or_insert(reason);
		self.local.close();
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
			}),
		}
	}

	/// The id the dialer chose.
	pub fn id(&self) -> &str {
		&self.inner.id
	}

	/// This side's broadcast, for tracks an application names itself.
	pub fn local(&self) -> &broadcast::Producer {
		&self.inner.local
	}

	/// The peer's broadcast, for tracks an application names itself.
	pub fn remote(&self) -> &broadcast::Consumer {
		&self.inner.remote
	}

	/// Open a uni stream named `name`: a track only this side writes.
	///
	/// Fails with [`Error::Duplicate`] when this side already opened a uni stream of
	/// that name.
	pub fn open_uni(&self, name: &str, info: impl Into<Option<track::Info>>) -> Result<track::Producer, Error> {
		self.inner
			.local
			.create_track(track_name(self.inner.role, Kind::Uni, name), info)
	}

	/// Open a bi stream named `name`, resolving once the peer accepts it.
	///
	/// `info` describes this side's track, and the peer's answer mirrors it. Fails with
	/// [`Error::Duplicate`] when this side already opened a bi stream of that name.
	pub fn open_bi(&self, name: &str, info: impl Into<Option<track::Info>>) -> Result<kio::Pending<OpenBi>, Error> {
		let name: Arc<str> = name.into();
		let producer = self
			.inner
			.local
			.create_track(track_name(self.inner.role, Kind::Bi, &name), info)?;
		self.inner.listing.lock().opening.insert(name.clone(), false);
		Ok(kio::Pending::new(OpenBi {
			inner: self.inner.clone(),
			name,
			producer: Some(producer),
		}))
	}

	/// Poll for the next uni stream the peer opened; `None` once the peer's broadcast ended.
	pub fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Option<Uni>, Error>> {
		let inner = &self.inner;
		let mut listing = inner.listing.lock();
		listing.drain(waiter, inner.role, &inner.remote);
		if let Some(name) = listing.uni.pop_front() {
			drop(listing);
			return Poll::Ready(
				match inner.remote.track(&track_name(inner.role.peer(), Kind::Uni, &name)) {
					Ok(consumer) => Ok(Some(Uni { name, consumer })),
					// The peer's broadcast ended.
					Err(Error::Unroutable) => Ok(None),
					Err(err) => Err(err),
				},
			);
		}
		match listing.ended() {
			Some(end) => Poll::Ready(end),
			None => Poll::Pending,
		}
	}

	/// Wait for the next uni stream the peer opened; see [`Self::poll_accept_uni`].
	#[cfg(feature = "async")]
	pub async fn accept_uni(&self) -> Result<Option<Uni>, Error> {
		kio::wait(|waiter| self.poll_accept_uni(waiter)).await
	}

	/// Poll for the next bi stream the peer opened, answering it with a track that mirrors
	/// the opener's info; `None` once the peer's broadcast ended.
	pub fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<Option<Bi>, Error>> {
		let inner = &self.inner;
		loop {
			let mut listing = inner.listing.lock();
			listing.drain(waiter, inner.role, &inner.remote);
			let Some((name, info)) = listing.bi.pop_front() else {
				return match listing.ended() {
					Some(end) => Poll::Ready(end),
					None => Poll::Pending,
				};
			};
			drop(listing);
			let track = track_name(inner.role.peer(), Kind::Bi, &name);
			let consumer = match inner.remote.track(&track) {
				Ok(consumer) => consumer,
				// The peer's broadcast ended.
				Err(Error::Unroutable) => return Poll::Ready(Ok(None)),
				Err(err) => return Poll::Ready(Err(err)),
			};
			// The opener's track info, minus how its copy reached this hop.
			let info = track::Info {
				properties: None,
				unchecked: false,
				..info
			};
			let producer = match inner.local.create_track(track, info) {
				Ok(producer) => producer,
				// The peer listed a stream again that this side already answered.
				Err(Error::Duplicate) => continue,
				Err(err) => return Poll::Ready(Err(err)),
			};
			return Poll::Ready(Ok(Some(Bi {
				name,
				producer,
				consumer,
			})));
		}
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

/// A bi stream being opened, from [`Socket::open_bi`]: resolves once the peer answers.
pub struct OpenBi {
	inner: Arc<Inner>,
	name: Arc<str>,
	/// This side's track, handed over once the peer answers.
	producer: Option<track::Producer>,
}

impl OpenBi {
	/// Poll for the peer's answer. Fails with [`Error::Unroutable`] once the peer's
	/// broadcast ends without one.
	pub fn poll_ok(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bi, Error>> {
		let inner = &self.inner;
		let mut listing = inner.listing.lock();
		listing.drain(waiter, inner.role, &inner.remote);
		if listing.opening.get(&self.name) == Some(&true) {
			listing.opening.remove(&self.name);
			drop(listing);
			let consumer = inner.remote.track(&track_name(inner.role, Kind::Bi, &self.name))?;
			let producer = self.producer.take().ok_or(Error::Closed)?;
			return Poll::Ready(Ok(Bi {
				name: self.name.clone(),
				producer,
				consumer,
			}));
		}
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

impl Drop for OpenBi {
	fn drop(&mut self) {
		if self.producer.is_some() {
			self.inner.listing.lock().opening.remove(&self.name);
		}
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
	/// A dialer that leaves before its broadcast resolves is skipped. Fails with
	/// [`Error::Closed`] once the origin closes.
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
				let name = track_name(opener, kind, "chat/room");
				assert_eq!(parse(&name), Some((opener, kind, "chat/room")));
			}
		}
		assert_eq!(track_name(Role::Dialer, Kind::Bi, "x"), ".dial/bi/x");
		assert_eq!(track_name(Role::Listener, Kind::Uni, "x"), ".accept/uni/x");
	}

	#[test]
	fn foreign_track_names_are_skipped() {
		for name in [
			"command",
			".dial",
			".dial/uni",
			".dial/other/x",
			".other/uni/x",
			"dial/uni/x",
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
