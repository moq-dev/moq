//! A broadcast is a named collection of tracks, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] creates tracks on demand: a [Consumer] subscribes by name, and the
//! producer either serves a track it already has or is handed a [`track::Request`] to
//! fill. Both handles are refcounted clones of one broadcast, which ends on
//! [`Producer::close`], when the last producer drops, or when the [`Finish`] that
//! [`Producer::finish`] returns drops, having kept its finished tracks up until their
//! media is stale.
//!
//! [Info] is the broadcast's static metadata, fixed for its lifetime.
use crate::{cache, stats, track};
use std::{
	collections::HashMap,
	sync::Arc,
	task::{Poll, ready},
	time::Duration,
};

use crate::Error;
use crate::origin::Route;

use super::origin_impl::Announcer;
use super::{Requests, WeakCache};

#[path = "listing.rs"]
mod listing;
// private: capability gap
// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
#[cfg(feature = "async")]
pub(crate) use listing::Owner;
pub(super) use listing::{ListOnAccept, Unlist};
pub use listing::{Lister, Tracks, TracksEvent};

/// A collection of media tracks that can be published and subscribed to.
///
/// Create via [`Info::produce`] to obtain both [`Producer`] and [`Consumer`] pair.
/// This is the broadcast's static identity, fixed for its lifetime.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Info {
	/// The cache pool this broadcast's tracks and groups inherit.
	pub pool: cache::Pool,

	/// Ceiling on each track's media-timestamp retention window.
	pub cache_duration: std::time::Duration,

	/// The path this broadcast is named by, which relative references in a catalog it
	/// serves (hang's `broadcast` field) resolve against.
	///
	/// [`origin::Producer::create_broadcast`](super::origin::Producer::create_broadcast) stamps
	/// the path the broadcast was created at, relative to the origin root (including through a
	/// scoped producer). Every [`Consumer`] an origin hands out is then re-stamped with the path
	/// *that handle* was requested or announced at, relative to its cursor's root, since the
	/// same broadcast can be reached under more than one name: a dynamic handler may serve a
	/// standalone broadcast at any path, and a rooted cursor names a broadcast more tightly
	/// than the origin does.
	///
	/// Empty (the default) for a standalone broadcast with no origin, which is then its own
	/// root: any `..` reference escapes.
	pub path: crate::PathOwned,

	/// The epoch of the route a [`Consumer`] was resolved through, naming the publisher
	/// instance it reads.
	///
	/// Stamped on every [`Consumer`] an origin resolves, like [`Self::path`]. `None` for a
	/// route without an epoch and for a handle not resolved through an origin.
	pub epoch: Option<crate::Epoch>,

	// What the frames this broadcast's groups are still receiving may allocate up front,
	// inherited from the origin handle that created it.
	// private: tuning
	pub(crate) frames: super::frame::Budget,
}

impl Default for Info {
	fn default() -> Self {
		Self {
			pool: cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY)),
			cache_duration: std::time::Duration::MAX,
			path: crate::PathOwned::default(),
			epoch: None,
			frames: Default::default(),
		}
	}
}

impl Info {
	/// Create a new broadcast with default metadata.
	pub fn new() -> Self {
		Self::default()
	}

	/// Consume this [Info] to create a producer that carries its metadata.
	///
	/// Keep the returned [`Producer`] alive for as long as the broadcast should stay
	/// available, and end it with [`Producer::close`]. See the note on [`Producer`].
	pub fn produce(self) -> Producer {
		Producer::new(self)
	}
}

#[derive(Default)]
struct BroadcastState {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	// Keyed by the track's shared `Arc<str>` name (the same Arc the handle holds).
	// The cache reclaims closed entries incrementally on insert so a long-lived
	// broadcast churning distinct track names stays bounded by the live count.
	tracks: WeakCache<Arc<str>, track::TrackWeak>,

	// Shared across suffixes and producer clones; cache eviction must not reset IDs.
	unique: u64,

	// Pending requests keyed by track name, coalescing concurrent `track()` calls
	// and waiting for a dynamic handler to accept or deny them. A request leaves
	// here once handed out (the handler caches it in `tracks`, so lookups keep
	// coalescing onto it there).
	requests: Requests<Arc<str>, track::Request>,

	// Each name's sequence namespace, outliving the producers that serve it: a name means
	// the same content for the broadcast's whole life, so a replacement producer that would
	// append a sequence an earlier one wrote is refused. Entries nothing holds or wrote are
	// swept as the map grows, so requests for names never served stay bounded.
	sequences: HashMap<Arc<str>, track::Sequence>,

	// The tracks the broadcast lists, and the readers listing them; see [`Tracks`].
	listing: listing::ListingState,

	// Set once the broadcast ends: `Producer::close()` or the last producer-side
	// handle dropping. Every lookup after it answers `Unroutable`.
	closing: bool,

	// Set by `Producer::finish`: only a finished broadcast can be superseded, since a live
	// one serves again once the newer route at its path withdraws.
	finished: bool,

	// Set once a finished broadcast's origin advertises a newer epoch at its path, which
	// ends its `Finish` at once.
	superseded: bool,
}

impl BroadcastState {
	/// Insert a track weak handle into the lookup, returning an error if a live
	/// track already holds the name. A closed entry under the name is reclaimed.
	fn insert_track(&mut self, weak: track::TrackWeak) -> Result<(), Error> {
		match self.tracks.insert(weak.name().clone(), weak) {
			Some(_) => Err(Error::Duplicate),
			None => Ok(()),
		}
	}

	/// The sequence namespace for `name`, shared with every earlier track of the name.
	fn sequence(&mut self, name: &Arc<str>) -> track::Sequence {
		if self.sequences.len() == self.sequences.capacity() {
			self.sequences.retain(|_, sequence| !sequence.is_unused());
		}
		self.sequences.entry(name.clone()).or_default().clone()
	}

	/// Resolve every name the broadcast never filled, so subscribers waiting on a
	/// [`track::Info`] that can no longer arrive fail with `err` instead of parking.
	///
	/// Covers a reservation nobody accepted and a request still queued for a handler.
	/// A request a [`Dynamic`] already took is left alone: it may be in flight to a peer
	/// still serving it, and the handler answers or drops it. So is a track that carries
	/// its info: an end there is that publisher's call, and its cache stays readable.
	fn reject_unserved(&mut self, err: Error) {
		for request in self.requests.drain_queued() {
			request.reject(err.clone());
		}
		for track in self.tracks.iter() {
			track.reject(err.clone());
		}
	}

	/// The track serving `name` now, published or requested, read without joining its demand.
	fn peek(&mut self, name: &str) -> Option<kio::ConsumerWeak<track::TrackState>> {
		if let Some(weak) = self.tracks.get(name) {
			return Some(weak.peek());
		}
		self.requests.get(name).map(|pending| pending.peek())
	}

	/// Live demand: a pending request or a consumed track. See [`Demand`].
	fn is_used(&self) -> bool {
		!self.requests.is_empty() || self.tracks.iter().any(|track| track.is_used())
	}

	/// Whether [`Self::is_used`] is `want`, parking `waiter` on the tracks in the way
	/// otherwise. The consumer counts live on the per-track channels, whose flips don't
	/// write this state, so each track's own answer decides: a re-read after the polls
	/// would see a reader that came or went since, with nothing parked to wake on its
	/// return.
	fn poll_demand(&self, waiter: &kio::Waiter, want: bool) -> Poll<()> {
		if want {
			// A closed track is never read again, so only an open, read one is demand.
			let used = !self.requests.is_empty()
				|| self
					.tracks
					.iter()
					.any(|track| matches!(track.poll_used(waiter), Poll::Ready(Ok(()))));
			return if used { Poll::Ready(()) } else { Poll::Pending };
		}
		if !self.requests.is_empty() {
			return Poll::Pending;
		}
		// Every other track is unused already, so the first one still read is what to
		// wait on: its last reader leaving wakes us to look again.
		match self.tracks.iter().all(|track| track.poll_unused(waiter).is_ready()) {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}
}

/// Manages tracks within a broadcast.
///
/// Create tracks up front with [Self::create_track], reserve a name to fill in
/// later with [Self::reserve_track], or handle on-demand consumer requests via
/// [Self::dynamic].
///
/// # Lifetime
///
/// A broadcast lives until [`Self::close`] or until the last [`Producer`] (or
/// [`Dynamic`]) drops, whichever comes first; both end it the same way. A publisher
/// whose input ended cleanly calls [`Self::finish`] instead, so a subscriber still
/// catching up can drain what it published. Children
/// do *not* keep it alive: cloning a [`Consumer`] or holding a [`track::Producer`]
/// does nothing for the broadcast's lifetime.
#[derive(Clone)]
pub struct Producer {
	// Held behind an Arc so each track born from this broadcast can inherit a shared
	// handle (threaded down by [`Self::create_track`] / [`Self::reserve_track`]).
	info: Arc<Info>,

	// Broadcast liveness, shared with every `Dynamic`. Consumers watch it (read-only)
	// for close; the guard ends the broadcast when the last of those handles drops.
	alive: Arc<Alive>,

	// Track registry plus the dynamic request queue, mutated by producers and
	// consumers alike under one lock.
	state: kio::Shared<BroadcastState>,

	// Ingress stats scope, set by a tagged `origin::Producer` at
	// `create_broadcast`. Inherited by the tracks this producer creates. Empty
	// (no-op) for an untagged broadcast.
	stats: stats::Scope,
}

impl Producer {
	/// Create a producer for the given broadcast metadata. Prefer [`Info::produce`].
	pub fn new(info: Info) -> Self {
		let state = kio::Shared::<BroadcastState>::default();
		Self {
			info: Arc::new(info),
			alive: Alive::new(state.clone()),
			state,
			stats: stats::Scope::default(),
		}
	}

	/// Attach an ingress stats scope, inherited by the tracks created on this
	/// broadcast. Set by a tagged `origin::Producer` at `create_broadcast`.
	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// Attach the advertisement of this broadcast's exact path. Set by
	/// `origin::Producer::create_broadcast`; a standalone broadcast has none.
	pub(super) fn with_announcer(self, announcer: Announcer) -> Self {
		*self.alive.announcer.lock() = Some(announcer);
		self
	}

	/// The route this broadcast is announced with, or `None` while it is not.
	pub fn route(&self) -> Option<Route> {
		self.alive.announcer.lock().as_ref()?.route()
	}

	/// Advertise this broadcast's exact path as a route, or replace the standing
	/// advertisement's route. The route is taken as given, epoch included: another
	/// epoch, or none, announces a new broadcast. To re-price the same instance,
	/// start from the current route:
	/// `broadcast.announce(broadcast.route().unwrap_or_default().with_cost(cost))`.
	///
	/// Until this is called the broadcast exists for nobody: announce cursors do
	/// not list it and requests for its path fail with [`Error::Unroutable`], for
	/// local consumers and peers alike. Call it once the tracks a subscriber needs
	/// first (a catalog) exist, so the advertisement lands with them in place:
	/// consumers act on it immediately. The route retracts on [`unannounce`](Self::unannounce),
	/// [`close`](Self::close), or the last producer dropping.
	///
	/// Fails with [`Error::Closed`] on a standalone broadcast (one not created
	/// through an origin, so there is nothing to announce into), on a claim answer
	/// still being [prepared](super::origin::Request::prepare), once the broadcast
	/// has closed, or once the origin's driver has been dropped.
	pub fn announce(&self, route: Route) -> Result<(), Error> {
		let mut announcer = self.alive.announcer.lock();
		let announcer = announcer.as_mut().ok_or(Error::Closed)?;
		announcer.announce(route)
	}

	/// Hand back an announcer withheld at creation and announce `route` once `admit`
	/// agrees, decided under the origin's table lock. The announcer is installed only
	/// when admitted; otherwise it drops, so this broadcast can never announce.
	pub(super) fn announce_withheld(
		&self,
		mut announcer: Announcer,
		route: Route,
		admit: impl FnOnce() -> bool,
	) -> Result<bool, Error> {
		let refused = {
			let mut slot = self.alive.announcer.lock();
			debug_assert!(slot.is_none(), "an announcer was already installed");
			// A close already retracted the slot, so an announcer installed now would
			// outlive the broadcast.
			if self.alive.state.lock().closing {
				Err(Error::Closed)
			} else {
				match announcer.announce_if(route, admit) {
					Ok(true) => {
						*slot = Some(announcer);
						return Ok(true);
					}
					refused => refused,
				}
			}
		};
		// Dropped outside the announcer lock, as in `Alive::retract`.
		drop(announcer);
		refused
	}

	/// Retract this broadcast's advertisement, if any, from local consumers and
	/// peers alike. New requests for the path fail with [`Error::Unroutable`] and
	/// the broadcast the origin served from it ends, while tracks already in
	/// flight carry on to their own end. [`announce`](Self::announce) brings it
	/// back.
	pub fn unannounce(&self) {
		self.alive.unannounce();
	}

	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// A watch-only handle to the broadcast's demand. See [`Demand`].
	pub fn demand(&self) -> Demand {
		Demand {
			alive: self.alive.token.consume().weak(),
			state: self.state.clone(),
		}
	}

	/// Produce a new track and insert it into the broadcast.
	///
	/// Pass a name and an optional [`track::Info`], so a bare name works:
	/// `create_track("video", None)` creates an untimed track.
	pub fn create_track(
		&self,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<track::Info>>,
	) -> Result<track::Producer, Error> {
		let name = name.into();
		let info = info.into().unwrap_or_default();
		let mut state = self.state.lock();

		// A consumer may have requested this name before it existed (a live
		// [`Dynamic`] queues such requests). Creating the track fulfills that
		// request: its consumers resolve against this very producer. Without
		// this they would be stranded, since the name is taken the moment the
		// track exists, so no handler could ever serve their queue entry.
		if let Some(request) = state.requests.take(name.as_ref()) {
			let track = request.with_stats(self.stats.clone()).accept(info.clone());
			// Cache it like a served request so concurrent lookups coalesce; a
			// live same-name entry cannot exist (its presence would have kept
			// the request from queuing).
			let _ = state.tracks.insert(name.clone(), track.weak());
			let unlist = Unlist::list(&self.state, &mut state.listing, name, &info);
			return Ok(track.listed(unlist));
		}

		let sequence = state.sequence(&name);
		let track = track::Producer::new(self.info.clone(), name.clone(), info.clone())
			.with_stats(self.stats.clone())
			.with_sequence(sequence);
		state.insert_track(track.weak())?;
		let unlist = Unlist::list(&self.state, &mut state.listing, name, &info);
		Ok(track.listed(unlist))
	}

	/// Reserve a track by name without finalizing its [`track::Info`].
	///
	/// Returns a [`track::Request`] already discoverable by consumers; call
	/// [`track::Request::accept`] to set its info and start producing. Use this when
	/// the producer can't pick the track's properties (e.g. timescale) until it has
	/// inspected the media, the same shape as a consumer-driven
	/// [`Dynamic::poll_requested_track`].
	///
	/// Subscribers wait on the name until it is accepted, so a reservation the producer
	/// ends up never filling has to be dropped or rejected. Ending the broadcast
	/// resolves whatever is left.
	pub fn reserve_track(&self, name: impl Into<Arc<str>>) -> Result<track::Request, Error> {
		let name = name.into();
		let mut state = self.state.lock();
		let request = track::Request::new(self.info.clone(), name.clone())
			.with_stats(self.stats.clone())
			.with_sequence(state.sequence(&name));
		state.insert_track(request.weak())?;
		Ok(request.listed(ListOnAccept {
			state: self.state.clone(),
			name,
		}))
	}

	/// Create a track with a unique name using the given suffix.
	///
	/// Uses [`Self::unique_name`]; minted names are never reused, even after closure.
	pub fn unique_track(&self, suffix: &str, info: impl Into<Option<track::Info>>) -> Result<track::Producer, Error> {
		let name = self.unique_name(suffix);
		self.create_track(name, info)
	}

	/// Generate a unique track name from a suffix without creating the track.
	///
	/// Returns `{id}{suffix}` with an increasing ID shared across all suffixes and
	/// producer clones in this broadcast, skipping names already in the lookup.
	/// A digit-leading suffix gets a `-` separator so it cannot be confused with the ID.
	/// Minted names are never reused, even if no track is created or it is closed.
	/// Explicit calls to [`Self::create_track`] can still reuse names.
	///
	/// # Panics
	///
	/// Panics if the broadcast exhausts its `u64` IDs.
	pub fn unique_name(&self, suffix: &str) -> String {
		let mut state = self.state.lock();
		let separator = if suffix.starts_with(|c: char| c.is_ascii_digit()) {
			"-"
		} else {
			""
		};
		loop {
			let id = state.unique;
			state.unique = id.checked_add(1).expect("unique track IDs exhausted");
			let name = format!("{id}{separator}{suffix}");
			if !state.tracks.contains_key(name.as_str()) {
				return name;
			}
		}
	}

	/// Create a dynamic producer that handles on-demand track requests from consumers.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(
			self.info.clone(),
			self.alive.clone(),
			self.state.clone(),
			self.stats.clone(),
		)
	}

	/// Create a consumer of this one publisher's broadcast.
	///
	/// A view of this broadcast object, not of its path: a new publisher at the same
	/// path never feeds it, so it ends when this broadcast does. Go through
	/// an origin for a consumer that should not care which publisher serves the path.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info.clone(),
			alive: self.alive.token.consume(),
			state: self.state.clone(),
			stats: stats::Scope::default(),
		}
	}

	/// End the broadcast for good, whether or not other clones are still alive.
	///
	/// Retracts its announcement and local discovery; a later [`Self::announce`] fails
	/// with [`Error::Closed`]. Tracks already handed out carry on and end with their
	/// own finish or abort. Every later [`Consumer::track`], and every name reserved or
	/// still queued for a handler, answers [`Error::Unroutable`]: the same answer an
	/// origin gives for a path nobody publishes. A request a [`Dynamic`] already took is
	/// left for that handler to answer.
	///
	/// Dropping the last producer does the same. Closing twice is a no-op.
	pub fn close(&self) {
		self.alive.close();
	}

	/// End the broadcast cleanly, keeping it servable until its media goes stale.
	///
	/// A subscriber still catching up, one that read the final catalog late or is draining a
	/// backlog, can still request and drain the finished tracks. The returned [`Finish`] keeps
	/// the broadcast announced and holds every track finished by now, so their producers can
	/// drop. A held track takes no more groups, not even one below its end, so nothing lands
	/// after the linger is fixed. Keep it for its [`linger`](Finish::linger), then drop it to [close](Self::close)
	/// the broadcast; `Finish::wait` does both on a runtime. Dropping it early is how an
	/// interrupt ends the linger, and a newer epoch at its path ends it too; see
	/// [`Finish::poll_closed`].
	pub fn finish(self) -> Finish {
		// Marked before asking the origin, so a newer epoch announced meanwhile either
		// supersedes the finished broadcast or is in the table by the time it is asked.
		self.state.lock().finished = true;
		let superseded = self.alive.announcer.lock().as_ref().is_some_and(Announcer::superseded);
		if superseded {
			self.state.lock().superseded = true;
		}

		let tracks: Vec<track::TrackWeak> = self.state.read().tracks.values().cloned().collect();
		// Held outside the lock: a track whose last producer drops meanwhile unlists itself
		// under it.
		let held: Vec<track::Hold> = tracks.iter().filter_map(track::TrackWeak::hold_finished).collect();
		let linger = held.iter().filter_map(|hold| hold.max_age).max().unwrap_or_default();
		Finish {
			producer: self,
			_held: held,
			linger,
		}
	}

	/// Whether any [`Consumer`] handle exists, unlike [`Self::demand`], which counts tracks.
	pub(super) fn is_held(&self) -> bool {
		self.alive.token.is_used()
	}

	/// `Ready(Ok)` once a [`Consumer`] handle exists, `Ready(Err)` once the broadcast ended.
	pub(super) fn poll_held(&self, waiter: &kio::Waiter) -> Poll<Result<(), kio::Closed>> {
		self.alive.token.poll_used(waiter)
	}

	/// `Ready(Ok)` once no [`Consumer`] handle is left, `Ready(Err)` once the broadcast ended.
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn poll_unheld(&self, waiter: &kio::Waiter) -> Poll<Result<(), kio::Closed>> {
		self.alive.token.poll_unused(waiter)
	}

	/// [`Self::close`], unless a [`Consumer`] holds the broadcast or a track it asked for
	/// still waits for a handler. Atomic with minting a consumer, so a holder either keeps
	/// the broadcast or only ever sees it closed. Returns whether the broadcast is closed.
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn close_unheld(&self) -> bool {
		self.alive.close_unheld()
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// A cleanly ended broadcast, still announced and serving its finished tracks.
///
/// Returned by [`Producer::finish`]. Dropping it closes the broadcast, then lets the held
/// tracks go, so a late lookup answers [`Error::Unroutable`].
pub struct Finish {
	producer: Producer,
	// Dropped after `Drop::drop` closes the broadcast.
	_held: Vec<track::Hold>,
	linger: Duration,
}

impl Finish {
	/// How long to keep this handle: the longest [`max_age`](track::Info::max_age) of the
	/// held tracks, each capped by the broadcast's [`cache_duration`](Info::cache_duration).
	/// Tracks still live, including one whose declared end the live edge has yet to reach
	/// or with a group still open, and tracks without a max-age, add nothing.
	pub fn linger(&self) -> Duration {
		self.linger
	}

	/// `Ready` once the broadcast closed elsewhere, or its origin advertises a newer
	/// [epoch](crate::origin::Route::epoch) at its path, so the linger is over early.
	///
	/// A request for the path resolves to the newer instance, so this one has nothing left
	/// to serve, and keeping it announced would bring it back if the newer one ends.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.producer.alive.token.poll_closed(waiter).is_ready() {
			return Poll::Ready(());
		}
		self.producer
			.state
			.poll(waiter, |state| match state.superseded {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.map(|_| ())
	}

	/// Keep the broadcast up for [`Self::linger`] on the ambient runtime's clock, or until
	/// [`Self::poll_closed`] ends it early, then close it. The linger counts from the first poll.
	#[cfg(feature = "async")]
	pub async fn wait(self) {
		let linger = Linger {
			finish: self,
			until: None,
		};
		crate::time::run(linger).await;
	}
}

impl Drop for Finish {
	fn drop(&mut self) {
		// Closed before `_held` drops, so no lookup finds a track after the linger.
		self.producer.close();
	}
}

/// Drives [`Finish::wait`] with the time it is polled with.
#[cfg(feature = "async")]
struct Linger {
	finish: Finish,
	// Set on the first poll; `Some(None)` when the linger overflows the clock.
	until: Option<Option<crate::time::Instant>>,
}

#[cfg(feature = "async")]
impl crate::time::Driver for Linger {
	fn poll(&mut self, now: crate::time::Instant, waiter: &kio::Waiter) -> Result<Option<crate::time::Instant>, Error> {
		if self.finish.poll_closed(waiter).is_ready() {
			return Err(Error::Closed);
		}
		let linger = self.finish.linger;
		match *self.until.get_or_insert_with(|| now.checked_add(linger)) {
			Some(until) if now >= until => Err(Error::Closed),
			until => Ok(until),
		}
	}
}

/// Ends the broadcast on [`Producer::close`] or when the last [`Producer`] or
/// [`Dynamic`] drops, closing the liveness channel every [`Consumer`] watches.
///
/// A refcount rather than a "am I the last one?" check inside `Drop`: that answer is
/// a snapshot, and acting on it is exactly what invalidates it.
struct Alive {
	token: kio::Producer<()>,
	state: kio::Shared<BroadcastState>,
	// The advertisement of the broadcast's exact path, owned here so it retracts
	// with the broadcast: on close or the last producer-side handle dropping. `None` for a standalone broadcast.
	announcer: kio::Lock<Option<Announcer>>,
}

impl Alive {
	fn new(state: kio::Shared<BroadcastState>) -> Arc<Self> {
		Arc::new(Self {
			token: kio::Producer::default(),
			state,
			announcer: kio::Lock::new(None),
		})
	}

	/// Withdraw the path's advertisement, if any; the broadcast stays alive.
	fn unannounce(&self) {
		if let Some(announcer) = self.announcer.lock().as_mut() {
			announcer.withdraw();
		}
	}

	/// End the broadcast. See [`Producer::close`].
	fn close(&self) {
		{
			let mut state = self.state.lock();
			if std::mem::replace(&mut state.closing, true) {
				return;
			}
			// A name that was reserved or queued but never served can't arrive now,
			// and `Consumer::track` answers `Unroutable` for one asked about after this
			// point. Say the same to whoever asked earlier.
			state.reject_unserved(Error::Unroutable);
			state.listing.close();
		}
		let _ = self.token.close();
		self.retract();
	}

	/// See [`Producer::close_unheld`].
	fn close_unheld(&self) -> bool {
		let token = {
			let mut state = self.state.lock();
			if state.closing {
				return true;
			}
			// A queued track is a reader on its way that let go of its handle.
			if !state.requests.is_empty() {
				return false;
			}
			// Held under the lock a consumer's first mint takes, so none appears until the
			// close below lands.
			let token = match self.token.write_unused() {
				kio::Unused::Idle(token) => token,
				kio::Unused::Used => return false,
				kio::Unused::Closed => return true,
			};
			state.closing = true;
			state.reject_unserved(Error::Unroutable);
			state.listing.close();
			token
		};
		token.close();
		self.retract();
		true
	}

	/// Drop the announcer for good, so a later `announce` fails with `Closed`. Dropped
	/// outside the announcer lock: the entry's removal re-syncs the origin's cursors under
	/// the origin's own lock.
	fn retract(&self) {
		let announcer = self.announcer.lock().take();
		drop(announcer);
	}
}

impl Drop for Alive {
	fn drop(&mut self) {
		self.close();
	}
}

/// Whether a path request still names the source that handed out its track.
#[derive(Clone, Default)]
#[cfg_attr(not(feature = "async"), allow(dead_code))]
// pending: /quest/m1/public-model/answer.md
pub(crate) enum RequestValidity {
	#[default]
	Unrestricted,
	Route(kio::Consumer<()>),
	Selected(std::sync::Arc<crate::model::origin_impl::Selection>),
	/// One period of a selection, captured by [`Self::start`].
	Period(kio::Consumer<()>, std::sync::Arc<crate::model::origin_impl::Selection>),
}

impl RequestValidity {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn token(route: Option<kio::Consumer<()>>) -> Self {
		route.map(Self::Route).unwrap_or_default()
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn for_source(route: &crate::origin::Dynamic, path: &crate::Path) -> Self {
		route
			.request_validity(path)
			.map(|selected| Self::Selected(std::sync::Arc::new(selected)))
			.unwrap_or_else(|| Self::Route(kio::Producer::new(()).consume()))
	}

	/// Capture one selection period so a pending request never revives after a replacement.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn start(&self) -> Self {
		match self {
			Self::Selected(selected) => Self::Period(selected.current(), selected.clone()),
			other => other.clone(),
		}
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	// pending: /quest/m1/public-model/answer.md
	pub(crate) fn check(&self, waiter: &kio::Waiter) -> Result<(), crate::Error> {
		match self {
			Self::Route(route) if route.poll_closed(waiter).is_ready() => Err(crate::Error::Unroutable),
			// A replacement retires the period only once its held `Restart` is released.
			Self::Period(route, selected)
				if route.poll_closed(waiter).is_ready() || selected.poll_retired(waiter).is_ready() =>
			{
				Err(crate::Error::Unroutable)
			}
			Self::Selected(_) => self.start().check(waiter),
			_ => Ok(()),
		}
	}
}

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl Producer {
	pub fn assert_create_track(
		&mut self,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<track::Info>>,
	) -> track::Producer {
		self.create_track(name, info).expect("should not have errored")
	}
}

/// A session-owned handle to a source broadcast created via
/// [`crate::origin::Producer::create_broadcast`], closing it on drop even while the
/// session's serve machines still hold its [`Dynamic`].
///
/// A peer's retraction and a dead session end the source the same way: a broadcast
/// carries no end cause past this hop. Shared by the lite and IETF subscribers.
#[cfg_attr(not(feature = "async"), allow(dead_code))]
// pending: /quest/m1/public-model/answer.md
pub(crate) struct SourceGuard(Producer);

impl SourceGuard {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub fn new(producer: Producer) -> Self {
		Self(producer)
	}
}

impl std::ops::Deref for SourceGuard {
	type Target = Producer;

	fn deref(&self) -> &Producer {
		&self.0
	}
}

impl Drop for SourceGuard {
	fn drop(&mut self) {
		self.0.close();
	}
}

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, the dynamic producer
/// picks up the request via [`Self::poll_requested_track`] and either
/// [`track::Request::accept`]s it with a concrete [`track::Info`] or
/// [`track::Request::reject`]s it. Dropped when no longer needed; pending requests
/// are automatically aborted.
#[derive(Clone)]
pub struct Dynamic {
	info: Arc<Info>,
	// Keeps the broadcast alive while a handler exists (mirrors a producer).
	alive: Arc<Alive>,
	state: kio::Shared<BroadcastState>,
	// Ingress stats scope, applied to the tracks this handler serves. Empty (no-op)
	// for an untagged broadcast.
	stats: stats::Scope,
	// Declared after `alive` so it drops second: when this was the broadcast's last
	// handle, `Alive` has already ended it and answered every queued request
	// `Unroutable`, so the handler's own `Dropped` rejection finds nothing left.
	_handler: Handler,
}

/// Counts one live [`Dynamic`], rejecting the queued requests when the last one drops.
struct Handler(kio::Shared<BroadcastState>);

impl Handler {
	fn new(state: kio::Shared<BroadcastState>) -> Self {
		state.lock().requests.add_handler();
		Self(state)
	}
}

impl Clone for Handler {
	fn clone(&self) -> Self {
		// Count each live handle, or dropping a clone would flip the handler count to
		// zero and future `track` calls would return `NotFound`.
		Self::new(self.0.clone())
	}
}

impl Drop for Handler {
	fn drop(&mut self) {
		// Decrement and reject under one lock, so a `track` call that saw a live
		// handler through the same lock can't slip a request past the rejection.
		let mut state = self.0.lock();
		if state.requests.remove_handler() {
			// No handlers left to fulfill pending requests; reject them so consumers
			// don't block forever on tracks nobody will serve.
			for request in state.requests.drain_queued() {
				request.reject(Error::Dropped);
			}
		}
	}
}

impl Dynamic {
	fn new(info: Arc<Info>, alive: Arc<Alive>, state: kio::Shared<BroadcastState>, stats: stats::Scope) -> Self {
		Self {
			info,
			alive,
			_handler: Handler::new(state.clone()),
			state,
			stats,
		}
	}

	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// Poll for the next consumer-requested track, without blocking.
	///
	/// Returns [`Error::Closed`] once the broadcast has ended, so a serving loop
	/// knows to stop and release its handle.
	pub fn poll_requested_track(&mut self, waiter: &kio::Waiter) -> Poll<Result<track::Request, Error>> {
		let mut state = ready!(self.state.poll(waiter, |state| {
			if state.requests.has_queued() || state.closing {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));

		if state.closing && !state.requests.has_queued() {
			return Poll::Ready(Err(Error::Closed));
		}

		let name = state.requests.pop().expect("predicate guaranteed a request");
		let pending = state.requests.remove(&name).expect("popped key must be pending");
		// Cache the served track so concurrent lookups coalesce onto it. If a live track already
		// holds the name (a publish raced the request), `insert` keeps it rather than shadowing it.
		let _ = state.tracks.insert(name, pending.weak());
		// Attribute the served track to this broadcast's ingress scope (no-op untagged).
		Poll::Ready(Ok(pending.claim().with_stats(self.stats.clone())))
	}

	/// Block until a consumer requests a track, returning a [`track::Request`] to serve.
	#[cfg(feature = "async")]
	pub async fn requested_track(&mut self) -> Result<track::Request, Error> {
		kio::wait(|waiter| self.poll_requested_track(waiter)).await
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info.clone(),
			alive: self.alive.token.consume(),
			state: self.state.clone(),
			stats: stats::Scope::default(),
		}
	}

	/// Answer the broadcast's track listings, for a handler that knows the tracks it
	/// would serve, such as a relay listing its upstream's. See [`Lister`].
	///
	/// A handler that serves any name on request takes none, so it lists nothing. One
	/// answer per broadcast: fails with [`Error::Duplicate`] while another lister exists.
	pub fn lister(&self) -> Result<Lister, Error> {
		Lister::new(self.state.clone())
	}

	/// Block until the broadcast ends, by [`Producer::close`] or every producer dropping.
	#[cfg(feature = "async")]
	pub async fn closed(&self) {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the producer closes.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.alive.token.poll_closed(waiter)
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl Dynamic {
	pub fn assert_request(&mut self) -> track::Request {
		kio::wait(|waiter| self.poll_requested_track(waiter))
			.now_or_never()
			.expect("should not have blocked")
			.expect("should not have errored")
	}

	pub fn assert_no_request(&mut self) {
		assert!(
			kio::wait(|waiter| self.poll_requested_track(waiter))
				.now_or_never()
				.is_none(),
			"should have blocked"
		);
	}
}

/// Subscribe to arbitrary broadcast/tracks.
///
/// Its close signal means this broadcast object ended, not that the path went
/// offline: announcements say whether a path is live, and a new publisher may
/// announce the same path again.
pub struct Consumer {
	info: Arc<Info>,
	// Broadcast liveness (read-only): watched for close.
	alive: kio::Consumer<()>,
	// Track registry plus request queue; a track handle's requests read the registry and enqueue.
	state: kio::Shared<BroadcastState>,
	// Egress stats scope, set by a tagged `origin::Consumer` at the broadcast
	// handoff. Inherited by the tracks subscribed through this handle. Empty (no-op)
	// for an untagged broadcast.
	stats: stats::Scope,
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			alive: self.alive.clone(),
			state: self.state.clone(),
			stats: self.stats.clone(),
		}
	}
}

impl Consumer {
	/// Attach an egress stats scope, inherited by the tracks subscribed through this
	/// handle. Set by a tagged `origin::Consumer` at the broadcast handoff.
	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// Stamp the path this handle was handed out at, overriding [`Info::path`].
	///
	/// The origin applies it to every broadcast it resolves, because the name belongs to
	/// the (broadcast, cursor) pair rather than to the broadcast: what a catalog's relative
	/// references resolve against is where the *reader* found the broadcast, not where its
	/// producer happened to create it. Free when the two already agree, which is the case
	/// for a broadcast created at the path an unrooted cursor asks for.
	pub(super) fn with_path(mut self, path: crate::PathOwned) -> Self {
		if self.info.path != path {
			let mut info = (*self.info).clone();
			info.path = path;
			self.info = Arc::new(info);
		}
		self
	}

	/// Stamp the epoch of the route this handle was resolved through, overriding [`Info::epoch`].
	pub(super) fn with_epoch(mut self, epoch: Option<crate::Epoch>) -> Self {
		if self.info.epoch != epoch {
			let mut info = (*self.info).clone();
			info.epoch = epoch;
			self.info = Arc::new(info);
		}
		self
	}

	/// The broadcast's metadata, as reached through this handle.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// Get a lazy handle to a track on this broadcast.
	///
	/// Nothing is requested until the handle queries, subscribes, or fetches, each of
	/// which looks the name up again. Fails with [`Error::Unroutable`] once the broadcast
	/// has ended, and [`Error::NotFound`] when nothing publishes the track and no
	/// [`Dynamic`] would serve a request for it.
	pub fn track(&self, name: &str) -> Result<track::Consumer, Error> {
		let current = {
			let mut state = self.state.lock();
			// An ended broadcast serves nothing new, not even a track it still has:
			// the lookup answers what a fresh `request_broadcast` for the path would.
			// Tracks already handed out are untouched.
			if state.closing {
				return Err(Error::Unroutable);
			}
			let current = state.peek(name);
			if current.is_none() && !state.requests.has_handlers() {
				return Err(Error::NotFound);
			}
			current
		};
		// Bound to *this* handle's view of the broadcast, so a catalog track resolves its
		// relative references against the path we were handed out at rather than the one
		// the producer was created at, and tagged with this broadcast's egress scope so its
		// subscriptions, fetches, and groups are attributed to the same broadcast.
		Ok(
			track::Consumer::lookup(name.into(), self.info.clone(), self.clone(), current)
				.with_stats(self.stats.clone()),
		)
	}

	/// Join the demand of the track serving `name`, requesting it from a [`Dynamic`]
	/// when nothing serves it yet. Registers the subscription before exposing demand.
	/// Errors as [`Self::track`] does.
	pub(super) fn resolve_track(
		&self,
		name: &str,
		subscription: Option<&track::Subscription>,
	) -> Result<(kio::Consumer<track::TrackState>, Option<track::Preferences>), Error> {
		let mut state = loop {
			let mut state = self.state.lock();

			if state.closing {
				return Err(Error::Unroutable);
			}

			// Reuse a live producer if one is already publishing the track. `get` drops a
			// closed entry and returns `None`, so we fall through to a fresh request.
			let Some(weak) = state.tracks.get(name) else {
				break state;
			};
			// Join outside the lock: the subscription and the first consumer wake the
			// serving session, which may re-enter this broadcast. The track's own lock
			// still orders the join against its idle teardown.
			drop(state);
			if let Some(resolved) = weak.try_consume(subscription) {
				return Ok(resolved);
			}
			// It closed between the liveness probe and the count bump (an idle teardown
			// committing under us). Look again: `get` reclaims it and the track is
			// requested again, rather than handing back a consumer of a dead track.
		};

		if let Some(pending) = state.requests.join(name) {
			// Coalesce onto a queued request for the same name. Joining under the lock
			// wakes no subscription observer: only a handler that popped the request
			// can watch its subscriptions, and popping moves it to `tracks`.
			return Ok(pending.join(subscription));
		}

		// Allocate the name once and share the same Arc across the request, the
		// requests map, and the FIFO order. The request inherits the broadcast's
		// cache pool through its `Arc<Info>`, same as a producer-created track.
		let name: Arc<str> = name.into();
		let request = track::Request::new(self.info.clone(), name.clone()).with_sequence(state.sequence(&name));
		let resolved = request.join(subscription);

		// With no handler alive to serve it, the request is dropped: `NotFound` beats
		// handing back a consumer that would only resolve `Dropped`.
		if state.requests.insert(name, request).is_err() {
			return Err(Error::NotFound);
		}

		Ok(resolved)
	}

	/// Read the track serving `name` now, without joining its demand or requesting it.
	/// Once the broadcast has ended, `ended` stands in for a track it no longer serves.
	pub(super) fn peek_track(
		&self,
		name: &str,
		ended: Option<&kio::ConsumerWeak<track::TrackState>>,
	) -> Option<kio::ConsumerWeak<track::TrackState>> {
		let mut state = self.state.lock();
		state.peek(name).or_else(|| ended.filter(|_| state.closing).cloned())
	}

	/// A watch-only handle to the broadcast's demand. See [`Demand`].
	///
	/// The consumer-side sibling of [`Producer::demand`], for a holder that has
	/// only a read handle. Holding this handle, or the [`Consumer`] it came from,
	/// is not itself demand.
	///
	/// Demand going away is [`Demand::poll_unused`] resolving. The broadcast going
	/// away is [`Error::Dropped`], which here means the upstream producer ended.
	pub fn demand(&self) -> Demand {
		Demand {
			alive: self.alive.weak(),
			state: self.state.clone(),
		}
	}

	/// Block until the broadcast ends, by [`Producer::close`] or every producer dropping.
	#[cfg(feature = "async")]
	pub async fn closed(&self) {
		self.alive.closed().await
	}

	/// Returns true once the broadcast has ended.
	pub fn is_closed(&self) -> bool {
		self.alive.is_closed()
	}

	/// Whether the broadcast has ended, observed under the state lock. The origin's
	/// dispatcher treats a rejection from such a source as imminent detach rather
	/// than a strike.
	pub(super) fn is_closing(&self) -> bool {
		self.state.read().closing
	}

	/// Register a [`kio::Waiter`] that fires when the broadcast closes.
	///
	/// Returns [`Poll::Ready`] if already closed, otherwise [`Poll::Pending`] after
	/// arming the waiter. Useful for composing close-detection into a larger poll
	/// without spawning a task per broadcast.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.alive.poll_closed(waiter)
	}

	/// Check if this is the exact same instance of a broadcast.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Create a weak reference that doesn't keep the broadcast alive.
	///
	/// Used to deduplicate dynamically-served broadcasts in the origin: a live weak yields
	/// a shared clone, a closed one is discarded so the next request re-serves.
	pub(super) fn weak(&self) -> WeakConsumer {
		WeakConsumer {
			info: self.info.clone(),
			alive: self.alive.weak(),
			state: self.state.clone(),
		}
	}
}

/// A weak reference to a broadcast that doesn't prevent it from closing.
///
/// Mirrors [`track::TrackWeak`]: held by the origin's dynamic cache to share one
/// dynamically-served broadcast across repeat requests without pinning it alive.
/// Only the `alive` handle needs to be weak; a [`kio::Shared`] carries no liveness,
/// so holding the state outright pins nothing.
#[derive(Clone)]
pub(super) struct WeakConsumer {
	info: Arc<Info>,
	alive: kio::ConsumerWeak<()>,
	state: kio::Shared<BroadcastState>,
}

impl WeakConsumer {
	/// Upgrade to a full [`Consumer`] sharing the same broadcast state.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info.clone(),
			alive: self.alive.consume(),
			state: self.state.clone(),
			stats: stats::Scope::default(),
		}
	}
}

impl super::WeakEntry for WeakConsumer {
	fn is_closed(&self) -> bool {
		self.alive.is_closed()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// A cloneable, watch-only handle to a broadcast's subscriber demand.
///
/// Obtained from [`Producer::demand`] or [`Consumer::demand`]; the broadcast-level sibling of
/// [`track::Demand`](crate::track::Demand). Demand means live interest in the
/// broadcast's content: a pending track request or a consumed track. A publisher
/// uses it to run expensive work only while someone is watching.
///
/// It's a weak handle: it neither keeps the broadcast alive nor counts as
/// demand itself. Once every producer is gone, [`poll_used`](Self::poll_used) /
/// [`poll_unused`](Self::poll_unused) return [`Error::Dropped`].
#[derive(Clone)]
pub struct Demand {
	alive: kio::ConsumerWeak<()>,
	state: kio::Shared<BroadcastState>,
}

impl Demand {
	/// Whether the broadcast has live demand right now.
	///
	/// A point-in-time snapshot with no registration; use [`Self::poll_used`] /
	/// [`Self::poll_unused`] (or their `poll_*` forms) to wait for the edge.
	pub fn is_used(&self) -> bool {
		self.state.read().is_used()
	}

	/// Block until the broadcast has demand. Resolves immediately if it already
	/// does; returns [`Error::Dropped`] once every producer is gone.
	#[cfg(feature = "async")]
	pub async fn used(&self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_used(waiter)).await
	}

	/// Block until the broadcast has no demand. Resolves immediately if it has
	/// none; returns [`Error::Dropped`] once every producer is gone.
	#[cfg(feature = "async")]
	pub async fn unused(&self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_unused(waiter)).await
	}

	/// Poll until a consumer is using this resource.
	pub fn poll_used(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_demand(waiter, true)
	}

	/// Poll until no consumer is using this resource.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_demand(waiter, false)
	}

	fn poll_demand(&self, waiter: &kio::Waiter, want: bool) -> Poll<Result<(), Error>> {
		// Closure is checked first, matching `track::Demand`: a dead broadcast
		// reports Dropped rather than pretending to answer.
		if self.alive.poll_closed(waiter).is_ready() {
			return Poll::Ready(Err(Error::Dropped));
		}
		let ready = self.state.poll(waiter, |state| state.poll_demand(waiter, want));
		match ready {
			Poll::Ready(_) => Poll::Ready(Ok(())),
			Poll::Pending => Poll::Pending,
		}
	}
}

impl Consumer {
	/// End the broadcast's [`Finish`] early, if it has one: its origin now advertises a
	/// newer epoch at its path.
	pub(super) fn supersede(&self) {
		let state = self.state.read();
		if state.finished && !state.superseded {
			drop(state);
			self.state.lock().superseded = true;
		}
	}
}

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl Consumer {
	pub fn assert_not_closed(&self) {
		assert!(
			kio::wait(|waiter| self.poll_closed(waiter)).now_or_never().is_none(),
			"should not be closed"
		);
	}

	pub fn assert_closed(&self) {
		assert!(
			kio::wait(|waiter| self.poll_closed(waiter)).now_or_never().is_some(),
			"should be closed"
		);
	}

	/// The tracks still open on the broadcast, whether or not anyone reads them.
	pub fn open_tracks(&self) -> usize {
		use super::WeakEntry;
		self.state
			.read()
			.tracks
			.iter()
			.filter(|track| !track.is_closed())
			.count()
	}
}

#[cfg(test)]
mod test {
	use super::*;
	#[cfg(feature = "async")]
	use std::time::Duration;

	#[test]
	fn a_subscription_is_registered_before_its_request_is_visible() {
		struct Serve {
			dynamic: std::sync::Mutex<Dynamic>,
			request: std::sync::Mutex<Option<track::Request>>,
			subscription: std::sync::Mutex<Option<Option<track::Subscription>>>,
		}
		impl std::task::Wake for Serve {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				let Poll::Ready(Ok(request)) = self.dynamic.lock().unwrap().poll_requested_track(&kio::Waiter::noop())
				else {
					return;
				};
				*self.subscription.lock().unwrap() = Some(request.subscription());
				*self.request.lock().unwrap() = Some(request);
			}
		}

		let producer = Info::new().produce();
		let serve = Arc::new(Serve {
			dynamic: std::sync::Mutex::new(producer.dynamic()),
			request: Default::default(),
			subscription: Default::default(),
		});
		let consumer = producer.consume().track("catalog").unwrap();
		let waiter = kio::Waiter::new(std::task::Waker::from(serve.clone()));
		assert!(serve.dynamic.lock().unwrap().poll_requested_track(&waiter).is_pending());
		let expected = track::Subscription::default().with_priority(3);
		let _pending = consumer.subscribe(expected.clone());
		assert_eq!(*serve.subscription.lock().unwrap(), Some(Some(expected)));
	}

	#[test]
	fn a_subscription_is_registered_before_its_demand_is_visible() {
		struct Inspect {
			track: track::Producer,
			seen: std::sync::Mutex<Option<Option<track::Subscription>>>,
		}
		impl std::task::Wake for Inspect {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				*self.seen.lock().unwrap() = Some(self.track.subscription());
			}
		}

		for lookup in [false, true] {
			let producer = Info::new().produce();
			let inspect = Arc::new(Inspect {
				track: producer.create_track("catalog", None).unwrap(),
				seen: Default::default(),
			});
			let waiter = kio::Waiter::new(std::task::Waker::from(inspect.clone()));
			assert_eq!(inspect.track.demand().poll_state(&waiter), track::DemandState::Idle);
			let consumer = if lookup {
				producer.consume().track("catalog").unwrap()
			} else {
				inspect.track.consume()
			};
			let expected = track::Subscription::default().with_priority(3);
			let _pending = consumer.subscribe(expected.clone());
			assert_eq!(*inspect.seen.lock().unwrap(), Some(Some(expected)), "lookup: {lookup}");
		}
	}

	#[test]
	fn a_subscription_wakes_its_observers_outside_the_broadcast_lock() {
		struct Reenter {
			broadcast: Consumer,
			woken: std::sync::atomic::AtomicBool,
		}
		impl std::task::Wake for Reenter {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				// Deadlocks if the subscribe that woke us still holds the broadcast lock.
				let _ = self.broadcast.track("other");
				self.woken.store(true, std::sync::atomic::Ordering::Relaxed);
			}
		}

		let producer = Info::new().produce();
		let mut track = producer.create_track("catalog", None).unwrap();
		let reenter = Arc::new(Reenter {
			broadcast: producer.consume(),
			woken: Default::default(),
		});
		let waiter = kio::Waiter::new(std::task::Waker::from(reenter.clone()));
		assert!(track.poll_subscription_changed(&waiter).is_pending());
		let consumer = producer.consume().track("catalog").unwrap();
		let _pending = consumer.subscribe(track::Subscription::default().with_priority(3));
		assert!(reenter.woken.load(std::sync::atomic::Ordering::Relaxed));
	}

	#[test]
	fn unique_names_are_never_reused() {
		let producer = Info::new().produce();
		let name = producer.unique_name(".opus");
		assert_eq!(name, "0.opus");
		let track = producer
			.create_track(name.clone(), crate::track::Info::timed())
			.unwrap();
		assert_eq!(producer.unique_name(".opus"), "1.opus");
		drop(track);
	}

	#[test]
	fn unique_names_survive_closed_track_pruning() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer.unique_track(".opus", crate::track::Info::timed()).unwrap();
		assert_eq!(track.name(), "0.opus");
		drop(track);
		assert!(matches!(consumer.track("0.opus"), Err(Error::NotFound)));
		assert_eq!(producer.unique_name(".opus"), "1.opus");
	}

	/// A track handle reads what an open broadcast serves now, so a track that went away
	/// reads empty rather than its stale cache.
	#[test]
	fn a_handle_forgets_a_track_its_open_broadcast_dropped() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer.create_track("a", crate::track::Info::timed()).unwrap();
		let _open = track.append_group().unwrap();
		let handle = consumer.track("a").unwrap();
		assert_eq!(handle.latest_sequence(), Some(0));
		drop(track);
		assert_eq!(handle.latest_sequence(), None);
	}

	/// A handle's largest reads the instance its open broadcast serves now, without asking
	/// for it, and ends once nothing is served under the name.
	#[test]
	fn a_handle_follows_the_largest_its_broadcast_serves() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer.create_track("a", crate::track::Info::default()).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(None, &b"a"[..]).unwrap();
		let mut handle = consumer.track("a").unwrap();
		let waiter = kio::Waiter::noop();
		assert_eq!(
			handle.poll_largest(&waiter),
			std::task::Poll::Ready(Some(crate::track::Position { group: 0, frame: 0 }))
		);
		assert!(handle.poll_largest(&waiter).is_pending(), "nothing newer yet");
		drop(group);
		drop(track);
		assert_eq!(handle.poll_largest(&waiter), std::task::Poll::Ready(None));
	}

	/// Once the broadcast ends, a handle keeps reading the track it saw when it was made.
	#[test]
	fn a_handle_keeps_its_track_once_the_broadcast_ends() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer.create_track("a", crate::track::Info::timed()).unwrap();
		let _open = track.append_group().unwrap();
		let handle = consumer.track("a").unwrap();
		producer.close();
		drop(track);
		assert_eq!(handle.latest_sequence(), Some(0));
	}

	#[test]
	fn unique_name_skips_a_live_collision() {
		let producer = Info::new().produce();
		let track = producer.create_track("0.opus", crate::track::Info::timed()).unwrap();
		assert_eq!(producer.unique_name(".opus"), "1.opus");
		drop(track);
		assert_eq!(producer.unique_name(".opus"), "2.opus");
	}

	#[test]
	fn unique_names_share_a_counter() {
		let producer = Info::new().produce();
		assert_eq!(producer.unique_name("-video"), "0-video");
		assert_eq!(producer.clone().unique_name("-audio"), "1-audio");
		assert_eq!(producer.unique_name("-video"), "2-video");
	}

	#[test]
	fn unique_names_separate_numeric_suffixes() {
		let producer = Info::new().produce();
		assert_eq!(producer.unique_name(""), "0");
		let name = producer.unique_name("2");
		assert_eq!(name, "1-2");
		for _ in 2..12 {
			producer.unique_name("");
		}
		assert_eq!(producer.unique_name(""), "12");
	}

	/// Await with a timeout so a missed demand wake fails the test instead of
	/// hanging it (time is paused, so the timeout fires instantly when idle).
	#[cfg(feature = "async")]
	async fn expect<T>(fut: impl Future<Output = T>) -> T {
		moq_clock::timeout(Duration::from_secs(1), fut)
			.await
			.expect("timed out waiting for a demand edge")
	}

	/// Demand on an ordinary broadcast tracks subscriber interest, not
	/// production: a live track producer alone is unused, and so is a bare track
	/// handle, a subscribed track is used, and both edges wake parked waiters.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn demand_ordinary() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let demand = producer.demand();

		// No demand yet; `unused` resolves immediately.
		assert!(!demand.is_used());
		demand.unused().await.unwrap();

		// Producing alone is not demand.
		let _track = producer.create_track("a", crate::track::Info::timed()).unwrap();
		assert!(!demand.is_used());

		// A bare handle is not demand.
		let track = consumer.track("a").unwrap();
		assert!(!demand.is_used());

		// A subscription appearing wakes a parked `used`.
		let mut handle = None;
		let (used, ()) = futures::join!(expect(demand.used()), async { handle = Some(track.subscribe(None)) });
		used.unwrap();
		assert!(demand.is_used());

		// The last consumer dropping wakes a parked `unused`.
		let (unused, ()) = futures::join!(expect(demand.unused()), async { drop(handle) });
		unused.unwrap();
		assert!(!demand.is_used());

		// Every producer gone: both edges report the closure.
		producer.close();
		assert!(matches!(demand.used().await, Err(Error::Dropped)));
		assert!(matches!(demand.unused().await, Err(Error::Dropped)));
	}

	/// One read track keeps the broadcast in demand however many others are unread, and
	/// its last reader leaving wakes a parked `unused`.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn demand_unused_waits_for_every_track() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let demand = producer.demand();
		let _video = producer.create_track("video", None).unwrap();
		let _audio = producer.create_track("audio", None).unwrap();

		let video = consumer.track("video").unwrap().subscribe(None);
		assert!(
			demand.poll_unused(&kio::Waiter::noop()).is_pending(),
			"a read track is demand"
		);

		let (unused, ()) = futures::join!(expect(demand.unused()), async { drop(video) });
		unused.unwrap();
	}

	/// A consumer demand handle distinguishes lost demand from a dropped producer.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn consumer_demand_reports_dropped_producer() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let watched = consumer.demand();
		let _video = producer.create_track("video", crate::track::Info::timed()).unwrap();

		let track = consumer.track("video").unwrap().subscribe(None);
		assert!(watched.is_used());

		let (unused, ()) = futures::join!(expect(watched.unused()), async { drop(track) });
		unused.unwrap();

		drop(producer);
		assert!(matches!(watched.used().await, Err(Error::Dropped)));
		assert!(matches!(watched.unused().await, Err(Error::Dropped)));
	}

	/// Subscribe and assert the result hasn't resolved yet (it stays pending until
	/// a publisher accepts). Returns the pending subscription to resolve after accepting.
	macro_rules! subscribe_pending {
		($consumer:expr, $name:expr) => {{
			let pending = $consumer.track($name).unwrap().subscribe(None);
			assert!(
				pending.poll_ok(&kio::Waiter::noop()).is_pending(),
				"subscribe should stay pending until the request is accepted"
			);
			pending
		}};
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn insert() {
		let mut producer = Info::new().produce();

		// Create the track before any consumer exists.
		let track1 = producer.assert_create_track("track1", crate::track::Info::timed());
		track1.append_group().unwrap().finish().unwrap();

		let consumer = producer.consume();

		// The track already exists, so subscribe resolves immediately.
		let mut track1_sub = consumer.track("track1").unwrap().subscribe(None).await.unwrap();
		track1_sub.assert_group();

		let track2 = producer.assert_create_track("track2", crate::track::Info::timed());

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.track("track2").unwrap().subscribe(None).await.unwrap();
		track2_consumer.assert_no_group();

		track2.append_group().unwrap().finish().unwrap();

		track2_consumer.assert_group();
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn closed() {
		let mut producer = Info::new().produce();
		let dynamic = producer.dynamic();

		let consumer = producer.consume();
		consumer.assert_not_closed();

		// Create a new track and insert it into the broadcast (resolves immediately).
		let track1 = producer.assert_create_track("track1", crate::track::Info::timed());
		let mut track1c = consumer.track("track1").unwrap().subscribe(None).await.unwrap();

		// A track nobody publishes stays pending until accepted.
		let track2_fut = subscribe_pending!(consumer, "track2");

		// Dropping the last dynamic handler rejects pending requests, but must NOT
		// cascade to externally-owned tracks.
		drop(dynamic);

		// track2 was a pending dynamic request, so its subscribe surfaces the rejection.
		assert!(track2_fut.await.is_err());

		// track1's producer is held outside the broadcast, so it survives.
		assert!(!track1.is_closed());
		track1c.assert_not_closed();
	}

	/// `close()` ends the broadcast for every clone at once, and a second close is a no-op.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_ends_every_clone() {
		let producer = Info::new().produce();
		let clone = producer.clone();
		let consumer = producer.consume();

		producer.close();
		consumer.closed().await;
		assert!(matches!(consumer.track("video"), Err(Error::Unroutable)));
		assert!(matches!(clone.consume().track("video"), Err(Error::Unroutable)));

		clone.close();
		producer.close();
	}

	/// Dropping the last producer ends the broadcast exactly like `close()`.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn drop_ends_like_close() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		drop(producer);
		consumer.closed().await;
		assert!(matches!(consumer.track("video"), Err(Error::Unroutable)));
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn requests() {
		let mut producer = Info::new().produce().dynamic();

		let consumer = producer.consume();
		let consumer2 = consumer.clone();

		// Two subscribers to the same name coalesce into one request.
		let track1_fut = subscribe_pending!(consumer, "track1");
		let track2_fut = subscribe_pending!(consumer2, "track1");

		// There should be exactly one request to serve.
		let request = producer.assert_request();
		producer.assert_no_request();
		assert_eq!(request.name(), "track1");

		// Accept it, which resolves both waiting subscribers.
		let track3 = request.accept(crate::track::Info::timed());
		let mut track1 = track1_fut.await.unwrap();
		let mut track2 = track2_fut.await.unwrap();

		track1.assert_not_closed();
		track1.assert_is_clone(&track2);
		track3.subscribe(None).assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().unwrap().finish().unwrap();
		track1.assert_group();
		track2.assert_group();

		// A pending request is cancelled when the dynamic producer is dropped.
		let track4_fut = subscribe_pending!(consumer, "track2");
		drop(producer);
		assert!(track4_fut.await.is_err());

		// With no dynamic producer left, requesting the handle fails outright.
		let track5 = consumer2.track("track3");
		assert!(track5.is_err(), "should have errored");
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn stale_producer() {
		let mut broadcast = Info::new().produce().dynamic();
		let consumer = broadcast.consume();

		// Subscribe to a track and serve it.
		let track1_fut = subscribe_pending!(consumer, "track1");
		let producer1 = broadcast.assert_request().accept(crate::track::Info::timed());
		let mut track1 = track1_fut.await.unwrap();

		// Close the producer (simulating publisher disconnect).
		producer1.append_group().unwrap().finish().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// The consumer should see the track as closed.
		track1.assert_closed();

		// Subscribe again to the same track: should get a NEW producer, not the stale one.
		let track2_fut = subscribe_pending!(consumer, "track1");
		let producer2 = broadcast.assert_request().accept(crate::track::Info::timed());
		let mut track2 = track2_fut.await.unwrap();
		track2.assert_not_closed();
		track2.assert_not_clone(&track1);

		// The new consumer should receive the new group, which continues past the first's.
		producer2
			.create_group(crate::group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		track2.assert_group();
	}

	/// A name keeps one sequence namespace for the broadcast's life. The application owns
	/// where a replacement starts: an append that would reuse an earlier producer's sequence,
	/// explicit writes included, is refused, while a new broadcast starts over.
	#[test]
	fn replacement_refuses_reused_sequences() {
		let producer = Info::new().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();

		let track = consumer.track("media").unwrap().subscribe(None);
		let mut first = dynamic.assert_request().accept(crate::track::Info::timed());
		assert_eq!(first.append_group().unwrap().sequence, 0);
		assert_eq!(first.append_datagram(crate::Timestamp::ZERO, b"x").unwrap(), 1);
		let _open = first.create_group(crate::group::Info { sequence: 8 }).unwrap();
		first.insert_datagram(12, crate::Timestamp::ZERO, b"x").unwrap();
		first.finish().unwrap();
		drop((first, track));

		let track = consumer.track("media").unwrap().subscribe(None);
		let mut second = dynamic.assert_request().accept(crate::track::Info::timed());
		assert!(matches!(second.append_group(), Err(Error::Duplicate)));
		assert!(matches!(
			second.append_datagram(crate::Timestamp::ZERO, b"x"),
			Err(Error::Duplicate)
		));

		// An explicit first write picks the start, and appends continue from there.
		let _open = second.create_group(crate::group::Info { sequence: 13 }).unwrap();
		assert_eq!(second.append_datagram(crate::Timestamp::ZERO, b"x").unwrap(), 14);
		second.finish().unwrap();
		drop((second, track));

		// A track the producer creates itself checks the same namespace.
		let third = producer.create_track("media", crate::track::Info::timed()).unwrap();
		assert!(matches!(third.append_group(), Err(Error::Duplicate)));
		assert_eq!(
			third
				.create_group(crate::group::Info { sequence: 15 })
				.unwrap()
				.sequence,
			15
		);

		let next = Info::new().produce();
		assert_eq!(
			next.create_track("media", crate::track::Info::timed())
				.unwrap()
				.append_group()
				.unwrap()
				.sequence,
			0
		);
	}

	/// A replacement continues the name's timeline, so its groups can't start before the
	/// highest one an earlier producer checked, while a new broadcast starts over.
	#[test]
	fn replacement_refuses_a_rewind_past_its_predecessor() {
		let at = |millis| crate::Timestamp::from_millis(millis).unwrap();
		let producer = Info::new().produce();

		let first = producer.create_track("media", crate::track::Info::timed()).unwrap();
		let mut group = first.append_group().unwrap();
		group.write_frame(at(1000), b"a".as_slice()).unwrap();
		group.finish().unwrap();
		drop((group, first));

		let second = producer.create_track("media", crate::track::Info::timed()).unwrap();
		let mut group = second.create_group(crate::group::Info { sequence: 1 }).unwrap();
		assert!(matches!(
			group.write_frame(at(500), b"b".as_slice()),
			Err(Error::TimestampMismatch)
		));
		group.write_frame(at(1000), b"b".as_slice()).unwrap();

		let next = Info::new().produce();
		let track = next.create_track("media", crate::track::Info::timed()).unwrap();
		track
			.append_group()
			.unwrap()
			.write_frame(at(500), b"c".as_slice())
			.unwrap();
	}

	/// Names requested but never served leave nothing behind, so a peer asking for
	/// arbitrary names cannot grow the broadcast.
	#[test]
	fn unserved_names_are_forgotten() {
		let producer = Info::new().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		for i in 0..1000 {
			let track = consumer.track(&i.to_string()).unwrap().subscribe(None);
			drop(dynamic.assert_request());
			drop(track);
		}
		assert!(producer.state.lock().sequences.len() < 16);
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn requested_unused() {
		let mut broadcast = Info::new().produce().dynamic();
		let bc = broadcast.consume();

		// Subscribe to a track that doesn't exist yet, then serve it.
		let c1_fut = subscribe_pending!(bc, "unknown_track");
		let producer1 = broadcast.assert_request().accept(crate::track::Info::timed());
		let consumer1 = c1_fut.await.unwrap();

		// The producer should NOT be unused yet because there's a consumer.
		assert!(producer1.demand().wanted().is_some(), "track producer should be used");

		// A second subscriber reuses the live producer (fast path / dedup).
		let consumer2 = bc.track("unknown_track").unwrap().subscribe(None).await.unwrap();
		consumer2.assert_is_clone(&consumer1);

		drop(consumer1);
		assert!(producer1.demand().wanted().is_some(), "track producer should be used");

		drop(consumer2);
		assert!(
			producer1.demand().wanted().is_none(),
			"track producer should be unused after all consumers are dropped"
		);

		// While the producer is still alive, re-subscribing to the same name reuses
		// it (no new request). This is what lets the relay linger upstream
		// subscriptions across transient consumer churn.
		let consumer3 = bc.track("unknown_track").unwrap().subscribe(None).await.unwrap();
		consumer3.assert_is_clone(&producer1.subscribe(None));
		broadcast.assert_no_request();
		drop(consumer3);

		// Aborting the producer closes its lookup entry; the next subscribe sees the
		// stale weak, evicts it, and creates a fresh request.
		producer1.abort(Error::Cancel).unwrap();

		let c4_fut = subscribe_pending!(bc, "unknown_track");
		let producer2 = broadcast.assert_request().accept(crate::track::Info::timed());
		let consumer4 = c4_fut.await.unwrap();
		drop(consumer4);
		assert!(
			producer2.demand().wanted().is_none(),
			"new track producer should be unused after its consumer is dropped"
		);
	}

	/// Creating a track a consumer already requested fulfills that request: the
	/// waiting subscriber resolves against the created producer, and no handler
	/// ever sees the (now-taken) name. Without this the requester is stranded:
	/// the name exists the moment the track does, so the queue entry could
	/// never be served under it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn create_track_fulfills_queued_request() {
		let producer = Info::new().produce();
		let mut dynamic = producer.dynamic();
		let bc = dynamic.consume();

		// Queue a request for a track that doesn't exist yet.
		let subscribing = subscribe_pending!(bc, "video");

		// The producer creates the track before any handler drains the queue.
		let track = producer.create_track("video", crate::track::Info::timed()).unwrap();
		let mut sub = subscribing.await.expect("fulfilled by create_track");

		// The fulfilled subscription is live against this very producer.
		track.append_group().unwrap().finish().unwrap();
		sub.recv_group().await.expect("recv").expect("group");

		// The handler never sees the request; a fresh subscribe reuses the track.
		dynamic.assert_no_request();
		let again = bc.track("video").unwrap().subscribe(None).await.unwrap();
		again.assert_is_clone(&track.subscribe(None));
	}

	// Cloning a `Dynamic` and dropping the clone must not flip the handler
	// count to zero. The relay's lite subscriber clones the
	// dynamic per spawned subscribe; if Clone skipped the increment, the
	// first finished subscribe would tear down the broadcast and any
	// follow-up `track` would return `NotFound`.
	#[test]
	fn dynamic_clone_keeps_alive() {
		let broadcast = Info::new().produce().dynamic();
		let consumer = broadcast.consume();

		let clone = broadcast.clone();
		drop(clone);

		// Original handle is still live, so the request registers (stays pending)
		// instead of failing with NotFound.
		let _fut = subscribe_pending!(consumer, "track1");
	}

	/// Closing an unheld broadcast yields to a consumer handle, and to a track a reader
	/// asked for before letting go, which no handler has taken yet. A handle minted from a
	/// weak once it closes sees it closed, so it never holds a broadcast that is ending.
	#[test]
	fn close_unheld_yields_to_holders() {
		let producer = Info::new().produce();
		let mut dynamic = producer.dynamic();
		let consumer = producer.consume();
		let weak = consumer.weak();
		assert!(producer.is_held());
		assert!(!producer.close_unheld(), "closed under a holder");

		let track = consumer.track("video").unwrap();
		drop(consumer);
		assert!(producer.is_held(), "a track handle holds its broadcast");
		let subscribing = track.subscribe(None);
		drop(track);
		drop(subscribing);
		assert!(!producer.is_held());
		assert!(!producer.close_unheld(), "closed under a track on its way");

		let request = dynamic.assert_request();
		drop(request);
		assert!(producer.close_unheld());
		assert!(weak.consume().is_closed());
		assert!(producer.close_unheld(), "closing twice is a no-op");
	}

	/// A reserved name nobody accepts is the parking case a publisher has to be able to
	/// end. Ending the broadcast is where it does: `Consumer::track` answers
	/// `Unroutable` for a name asked about after this point, so whoever asked earlier gets
	/// the same answer instead of waiting on info that can never arrive.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_resolves_a_reserved_name() {
		let producer = Info::new().produce();
		let consumer = producer.consume();

		let _request = producer.reserve_track("track1").unwrap();
		let pending = subscribe_pending!(consumer, "track1");

		producer.close();
		assert!(matches!(pending.await, Err(Error::Unroutable)));
	}

	/// A request still queued for a handler is the same parking case reached from the
	/// consumer side, so it ends the same way.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_resolves_a_queued_request() {
		let producer = Info::new().produce();
		let dynamic = producer.dynamic();
		let consumer = dynamic.consume();

		let pending = subscribe_pending!(consumer, "track1");

		producer.close();
		assert!(matches!(pending.await, Err(Error::Unroutable)));
		drop(dynamic);
	}

	/// A queued request answers the same when the broadcast ends by its last handle
	/// dropping, even when that handle is the `Dynamic` that would have served it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_the_last_handle_resolves_a_queued_request() {
		let dynamic = Info::new().produce().dynamic();
		let consumer = dynamic.consume();

		let pending = subscribe_pending!(consumer, "track1");

		drop(dynamic);
		assert!(matches!(pending.await, Err(Error::Unroutable)));
	}

	/// With a producer still alive, losing the last handler is not the broadcast ending:
	/// the queued request fails as `Dropped`.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_the_last_handler_resolves_a_queued_request_dropped() {
		let producer = Info::new().produce();
		let dynamic = producer.dynamic();
		let consumer = dynamic.consume();

		let pending = subscribe_pending!(consumer, "track1");

		drop(dynamic);
		assert!(matches!(pending.await, Err(Error::Dropped)));
		producer.close();
	}

	/// A request a handler already took is the handler's to answer: it may be in flight
	/// to a peer, and a retraction does not disturb subscriptions already in flight.
	/// Whatever the handler decides still reaches the consumer.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_leaves_a_claimed_request_to_its_handler() {
		let producer = Info::new().produce();
		let mut dynamic = producer.dynamic();
		let consumer = dynamic.consume();

		let accepted = subscribe_pending!(consumer, "track1");
		let request = dynamic.requested_track().await.unwrap();
		let dropped = subscribe_pending!(consumer, "track2");
		let abandoned = dynamic.requested_track().await.unwrap();

		producer.close();
		assert!(
			accepted.poll_ok(&kio::Waiter::noop()).is_pending(),
			"close rejected a claimed request"
		);
		assert!(
			dropped.poll_ok(&kio::Waiter::noop()).is_pending(),
			"close rejected a claimed request"
		);

		let _track = request.accept(crate::track::Info::timed());
		assert!(accepted.await.is_ok(), "the handler's accept reaches the consumer");
		drop(abandoned);
		assert!(dropped.await.is_err(), "the handler dropping it rejects the consumer");
		drop(dynamic);
	}

	/// A reverse fetch can install the track metadata before the live request is
	/// accepted, but it does not create a live publisher. Closing the broadcast
	/// must still reject that name so an arrival-order subscriber does not park on
	/// backfill that is deliberately absent from its queue.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_resolves_an_unaccepted_track_with_fetched_info() {
		let producer = Info::new().produce();
		let consumer = producer.consume();

		let request = producer.reserve_track("track1").unwrap();
		let dynamic = request.dynamic();
		let track = consumer.track("track1").unwrap();
		let pending_fetch = track.fetch_group(0, None);
		let fetch = dynamic.requested_group().await.unwrap();
		let mut group = fetch.accept(crate::track::Info::timed()).unwrap();
		group.finish().unwrap();
		pending_fetch.await.unwrap();

		let mut subscriber = track.subscribe(None).await.unwrap();
		producer.close();
		assert!(matches!(subscriber.recv_group().await, Err(Error::Unroutable)));

		let stale = request.accept(crate::track::Info::timed());
		assert!(stale.append_group().is_err());
	}

	/// A staged track resolves no subscriber until it commits, and then with what was
	/// written meanwhile.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_staged_track_resolves_subscribers_at_commit() {
		let producer = Info::new().produce();
		let request = producer.reserve_track("track1").unwrap();
		let track = producer.consume().track("track1").unwrap();
		let mut subscribing = std::pin::pin!(track.subscribe(None));
		assert!(futures::poll!(subscribing.as_mut()).is_pending());

		let mut staged = request.stage(crate::track::Info::timed());
		staged.write_frame(crate::Timestamp::ZERO, b"x".as_slice()).unwrap();
		moq_clock::sim::yield_now().await;
		assert!(futures::poll!(subscribing.as_mut()).is_pending(), "still staged");

		let _track = staged.commit();
		let mut subscriber = subscribing.await.unwrap();
		assert_eq!(subscriber.recv_group().await.unwrap().unwrap().sequence, 0);
	}

	/// Dropping a staged track uncommitted rejects the request, so its subscribers fail
	/// rather than wait on a track nobody will publish.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_a_staged_track_rejects_the_request() {
		let producer = Info::new().produce();
		let request = producer.reserve_track("track1").unwrap();
		let track = producer.consume().track("track1").unwrap();
		let subscribing = track.subscribe(None);

		let mut staged = request.stage(crate::track::Info::timed());
		staged.write_frame(crate::Timestamp::ZERO, b"x".as_slice()).unwrap();
		drop(staged);
		assert!(matches!(subscribing.await, Err(Error::Dropped)));
	}

	/// Dropping a staged track uncommitted aborts a group still being written through it, so
	/// its writer fails rather than fill a track nobody will read.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_a_staged_track_aborts_its_open_groups() {
		let producer = Info::new().produce();
		let request = producer.reserve_track("track1").unwrap();
		let staged = request.stage(crate::track::Info::timed());
		let mut group = staged.append_group().unwrap();
		drop(staged);
		let write = group.write_frame(crate::Timestamp::ZERO, b"x".as_slice());
		assert!(matches!(write, Err(Error::Dropped)), "{write:?}");
	}

	/// Ending the broadcast doesn't cascade into a track someone is publishing: it keeps
	/// its cache and its publisher decides when it ends. Only a new lookup is refused.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_spares_a_served_track() {
		let producer = Info::new().produce();
		let consumer = producer.consume();

		let track = producer.create_track("track1", crate::track::Info::timed()).unwrap();
		let mut subscriber = consumer.track("track1").unwrap().subscribe(None).await.unwrap();

		producer.close();
		assert!(matches!(consumer.track("track1"), Err(Error::Unroutable)));

		track.append_group().unwrap().finish().unwrap();
		subscriber.assert_group();
		track.finish().unwrap();
	}

	/// The publisher may still be holding the `track::Request` for a name the broadcast
	/// just gave up on. Accepting it afterwards must not resurrect the track, or a
	/// subscriber that was told `Unroutable` could be contradicted by a later one.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn close_leaves_a_stale_reservation_inert() {
		let producer = Info::new().produce();
		let consumer = producer.consume();

		let request = producer.reserve_track("track1").unwrap();
		let pending = subscribe_pending!(consumer, "track1");

		producer.close();
		assert!(matches!(pending.await, Err(Error::Unroutable)));

		let track = request.accept(crate::track::Info::timed());
		assert!(track.append_group().is_err());
		let mut subscriber = track.subscribe(None);
		assert!(matches!(subscriber.recv_group().await, Err(Error::Unroutable)));
		assert!(consumer.track("track1").is_err());
	}

	/// Dropping a `track::Request` is not a verdict about the name, so it resolves as
	/// `Dropped` (a handler lost to a crashed publisher or a dead transport), never as
	/// `NotFound`. Only an explicit rejection may claim the track is absent.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_a_reserved_request_resolves_dropped() {
		let producer = Info::new().produce();
		let consumer = producer.consume();

		let request = producer.reserve_track("track1").unwrap();
		let pending = subscribe_pending!(consumer, "track1");

		drop(request);
		assert!(matches!(pending.await, Err(Error::Dropped)));
		producer.close();
	}

	/// `track::Request::reject` carries its reason the same way, which is what lets a
	/// subscriber tell "no such track" from "the publisher went away".
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn rejecting_a_reserved_request_carries_the_reason() {
		let producer = Info::new().produce();
		let consumer = producer.consume();

		let request = producer.reserve_track("track1").unwrap();
		let pending = subscribe_pending!(consumer, "track1");

		request.reject(Error::NotFound);
		assert!(matches!(pending.await, Err(Error::NotFound)));
		producer.close();
	}

	/// The interleave every unused-driven teardown has to survive: a wire subscriber
	/// observes zero consumers, and a viewer looks the track up again before the
	/// teardown commits. The returning viewer keeps the track alive, and once it
	/// really does commit the cached handle is reclaimed rather than handed out
	/// cancelled.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_idle_teardown_yields_to_a_returning_viewer() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer.create_track("video", crate::track::Info::timed()).unwrap();

		// The unused wake a teardown acts on.
		assert!(track.demand().wanted().is_none());

		// Demand returns in the gap before it commits.
		let viewer = consumer.track("video").unwrap().subscribe(None);
		let track = track
			.abort_unused(Error::Cancel)
			.expect_err("viewer keeps the track alive");

		// So the viewer holds a live track, not a cancelled one.
		assert!(!track.is_closed());
		let mut subscriber = viewer.await.unwrap();
		subscriber.assert_no_group();
		track.append_group().unwrap().finish().unwrap();
		assert!(subscriber.recv_group().await.unwrap().is_some());

		// Once the viewer really leaves, the same teardown commits, and the lookup
		// re-requests the track instead of resolving the closed one.
		drop(subscriber);
		assert!(track.abort_unused(Error::Cancel).is_ok());
		assert!(matches!(consumer.track("video"), Err(Error::NotFound)));

		producer.close();
	}

	#[test]
	fn abort_unused_accepts_an_already_closed_track_with_consumers() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer.create_track("video", crate::track::Info::timed()).unwrap();
		let _viewer = consumer.track("video").unwrap().subscribe(None);
		assert!(track.demand().is_used());
		track.clone().abort(Error::Cancel).unwrap();
		assert!(!track.demand().is_used());
		assert!(track.abort_unused(Error::Cancel).is_ok());
		producer.close();
	}

	/// A finished broadcast keeps serving its finished tracks after their producers drop,
	/// so a late subscriber still gets the media, and closes once the longest max-age passes.
	/// A live track, a track whose declared end is still ahead of its live edge, and a track
	/// without a max-age add nothing to the wait.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn finish_serves_finished_tracks_until_stale() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let secs = |secs| crate::track::Info::timed().with_max_age(Duration::from_secs(secs));
		let video = producer.create_track("video", secs(2)).unwrap();
		let audio = producer.create_track("audio", secs(5)).unwrap();
		let catalog = producer.create_track("catalog", None).unwrap();
		let live = producer.create_track("live", secs(60)).unwrap();
		let mut tail = producer.create_track("tail", secs(60)).unwrap();
		for track in [&video, &audio, &catalog, &tail] {
			track.append_group().unwrap().finish().unwrap();
		}
		for track in [&video, &audio, &catalog] {
			track.finish().unwrap();
		}
		tail.finish_at(crate::track::Position::group(2)).unwrap();

		let start = moq_clock::now();
		let finish = producer.finish();
		assert_eq!(finish.linger(), Duration::from_secs(5));
		let finish = moq_clock::sim::spawn(finish.wait());
		drop((video, audio, catalog));

		moq_clock::sleep(Duration::from_secs(4)).await;
		let mut late = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
		late.assert_group();
		assert!(!consumer.is_closed());

		finish.await.unwrap();
		assert_eq!(moq_clock::now() - start, Duration::from_secs(5));
		assert!(consumer.is_closed());
		assert!(matches!(consumer.track("audio"), Err(Error::Unroutable)));
		live.finish().unwrap();
		drop(tail);
	}

	/// Dropping the handle, the library's interrupt, closes the broadcast at once, even
	/// while another producer handle is alive.
	#[test]
	fn dropping_finish_closes_early() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer
			.create_track(
				"video",
				crate::track::Info::timed().with_max_age(Duration::from_secs(30)),
			)
			.unwrap();
		track.finish().unwrap();

		let finish = producer.clone().finish();
		assert!(!consumer.is_closed());
		drop(finish);
		assert!(consumer.is_closed());
	}

	/// The linger is the local retention window, so an origin's `cache_duration` below a
	/// track's max-age ends it sooner.
	#[test]
	fn finish_clamps_the_linger_to_the_cache_duration() {
		let producer = Info {
			cache_duration: Duration::from_secs(5),
			..Default::default()
		}
		.produce();
		let secs = |secs| crate::track::Info::timed().with_max_age(Duration::from_secs(secs));
		let tracks = [("video", 30), ("audio", 2)].map(|(name, age)| producer.create_track(name, secs(age)).unwrap());
		for track in &tracks {
			track.finish().unwrap();
		}
		assert_eq!(producer.finish().linger(), Duration::from_secs(5));
	}

	/// A track with a group still open takes frames past the linger, so it is not held.
	#[test]
	fn finish_skips_a_track_with_an_open_group() {
		let producer = Info::new().produce();
		let track = producer
			.create_track(
				"video",
				crate::track::Info::timed().with_max_age(Duration::from_secs(30)),
			)
			.unwrap();
		let _group = track.append_group().unwrap();
		track.finish().unwrap();
		assert_eq!(producer.finish().linger(), Duration::ZERO);
	}

	/// A held track takes no more groups, even one below its end, so nothing lands after
	/// the linger was fixed.
	#[test]
	fn finish_seals_the_held_tracks() {
		let producer = Info::new().produce();
		let track = producer
			.create_track(
				"video",
				crate::track::Info::timed().with_max_age(Duration::from_secs(30)),
			)
			.unwrap();
		track
			.create_group(crate::group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		track.finish().unwrap();
		track
			.create_group(crate::group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();

		let _finish = producer.finish();
		assert!(matches!(
			track.create_group(crate::group::Info { sequence: 0 }),
			Err(Error::Closed)
		));
	}

	/// A caller without a runtime keeps the handle until its own clock passes the linger,
	/// serving the finished track until then, and drops it to close the broadcast.
	#[test]
	fn a_runtime_free_caller_drops_finish_on_its_own_clock() {
		let producer = Info::new().produce();
		let consumer = producer.consume();
		let track = producer
			.create_track(
				"video",
				crate::track::Info::timed().with_max_age(Duration::from_secs(3)),
			)
			.unwrap();
		track.append_group().unwrap().finish().unwrap();
		track.finish().unwrap();

		let start = crate::time::Instant::now();
		let finish = producer.finish();
		drop(track);
		let deadline = start + finish.linger();
		let mut now = start;
		while now < deadline {
			assert!(finish.poll_closed(&kio::Waiter::noop()).is_pending());
			assert!(consumer.track("video").is_ok(), "served {:?} in", now - start);
			now += Duration::from_secs(1);
		}
		drop(finish);
		assert!(consumer.is_closed());
		assert!(matches!(consumer.track("video"), Err(Error::Unroutable)));
	}

	/// A close elsewhere ends the linger early, for a poller and for [`Finish::wait`].
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn closing_elsewhere_ends_the_linger() {
		let producer = Info::new().produce();
		let track = producer
			.create_track(
				"video",
				crate::track::Info::timed().with_max_age(Duration::from_secs(30)),
			)
			.unwrap();
		track.finish().unwrap();
		let other = producer.clone();
		let finish = producer.finish();
		assert!(finish.poll_closed(&kio::Waiter::noop()).is_pending());

		let start = moq_clock::now();
		let wait = moq_clock::sim::spawn(finish.wait());
		moq_clock::sleep(Duration::from_secs(1)).await;
		other.close();
		wait.await.unwrap();
		assert_eq!(moq_clock::now() - start, Duration::from_secs(1));
	}
}
