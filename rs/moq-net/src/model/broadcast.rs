//! A broadcast is a named collection of tracks, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] creates tracks on demand: a [Consumer] subscribes by name, and the
//! producer either serves a track it already has or is handed a [`track::Request`] to
//! fill. Both handles are refcounted clones of one broadcast, which ends on
//! [`Producer::close`] or when the last producer drops. A broadcast has no finished state:
//! a publisher ends its content by finishing its tracks, and goes offline by closing.
//!
//! [Info] is the broadcast's static metadata, fixed for its lifetime.
use crate::{cache, stats, track};
use std::{
	collections::HashSet,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Poll, ready},
};

use crate::Error;
use crate::origin::Route;

use super::origin_impl::Announcer;
use super::{RequestQueue, WeakCache, WeakEntry};

#[path = "listing.rs"]
mod listing;
// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
#[cfg(feature = "async")]
pub(crate) use listing::Owner;
pub(super) use listing::{ListOnAccept, Unlist};
// Only sessions answer a peer's listings.
pub(crate) use listing::Lister;
pub use listing::{Tracks, TracksEvent};

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
	pub path: crate::Path,

	/// The epoch of the route a [`Consumer`] was resolved through, naming the publisher
	/// instance it reads.
	///
	/// Stamped on every [`Consumer`] an origin resolves, like [`Self::path`]. `None` for a
	/// route without an epoch and for a handle not resolved through an origin.
	pub epoch: Option<crate::Epoch>,

	// What the frames this broadcast's groups are still receiving may allocate up front,
	// inherited from the origin handle that created it.
	pub(crate) frames: super::frame::Budget,
}

impl Default for Info {
	fn default() -> Self {
		Self {
			pool: cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY)),
			cache_duration: std::time::Duration::MAX,
			path: crate::Path::default(),
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
	requests: RequestQueue<Arc<str>, track::Request>,

	// The tracks the broadcast lists, and the readers listing them; see [`Tracks`].
	listing: listing::ListingState,

	// Set once the broadcast ends: `Producer::close()` or the last producer-side
	// handle dropping. Every lookup after it answers `Unroutable`.
	closing: bool,

	// The tracks cached since each [`Demand`] handle last looked, by handle, so a
	// handle watches each track once rather than rescanning the table.
	demands: slab::Slab<Vec<track::TrackWeak>>,
}

impl BroadcastState {
	/// Insert a track weak handle into the lookup, returning an error if a live
	/// track already holds the name. A closed entry under the name is reclaimed.
	fn insert_track(&mut self, weak: track::TrackWeak) -> Result<(), Error> {
		match self.cache_track(weak) {
			Some(_) => Err(Error::Duplicate),
			None => Ok(()),
		}
	}

	/// Cache a track under its name and hand it to every [`Demand`], unless a live track
	/// already holds the name, which is returned instead.
	fn cache_track(&mut self, weak: track::TrackWeak) -> Option<track::TrackWeak> {
		if let Some(live) = self.tracks.insert(weak.name().clone(), weak.clone()) {
			return Some(live);
		}
		for (_, cached) in &mut self.demands {
			// A handle that stops polling must not pin every track it was handed. Headroom
			// for as many tracks as survive defers the next scan until the inbox doubles,
			// so a full inbox losing one track doesn't rescan it on every insert.
			if cached.len() == cached.capacity() {
				cached.retain(|track| !track.is_closed());
				cached.reserve(cached.len());
			}
			cached.push(weak.clone());
		}
		None
	}

	/// Resolve every name the broadcast never filled, so subscribers waiting on a
	/// [`track::Info`] that can no longer arrive fail with `err` instead of parking.
	///
	/// Covers a reservation nobody accepted and a request still queued for a handler.
	/// A request a [`Requests`] already took is left alone: it may be in flight to a peer
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

	/// End every listing, reporting each listed track whose producer declared its end as
	/// finished, though that producer still holds it in the table.
	fn close_listing(&mut self) {
		let finished: HashSet<Arc<str>> = self
			.tracks
			.iter()
			.filter(|track| track.is_finished())
			.map(|track| track.name().clone())
			.collect();
		self.listing.close(|name| finished.contains(name));
	}

	/// The track serving `name` now, published or requested, read without joining its demand.
	fn peek(&mut self, name: &str) -> Option<kio::ConsumerWeak<track::TrackState>> {
		if let Some(weak) = self.tracks.get(name) {
			return Some(weak.peek());
		}
		self.requests.get(name).map(|pending| pending.peek())
	}
}

/// Manages tracks within a broadcast.
///
/// Create tracks up front with [Self::create_track], reserve a name to fill in
/// later with [Self::reserve_track], or handle on-demand consumer requests via
/// [Self::requests].
///
/// # Lifetime
///
/// A broadcast lives until [`Self::close`] or until the last [`Producer`] (or
/// [`Requests`]) drops, whichever comes first; both end it the same way. A publisher
/// whose input ended cleanly finishes its tracks first: a subscriber still catching up
/// drains them after the close. Children
/// do *not* keep it alive: cloning a [`Consumer`] or holding a [`track::Producer`]
/// does nothing for the broadcast's lifetime.
#[derive(Clone)]
pub struct Producer {
	// Held behind an Arc so each track born from this broadcast can inherit a shared
	// handle (threaded down by [`Self::create_track`] / [`Self::reserve_track`]).
	info: Arc<Info>,

	// Broadcast liveness, shared with every `Requests`. Consumers watch it (read-only)
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

	/// Answer track requests only while `validity` holds, ending the broadcast once it
	/// lapses. Set when a session mints a source, before any handle is shared.
	pub(crate) fn with_validity(mut self, validity: RequestValidity) -> Self {
		Arc::get_mut(&mut self.alive)
			.expect("validity is set on a fresh broadcast")
			.validity = validity;
		self
	}

	/// The route this broadcast is announced with, or `None` while it is not.
	pub(super) fn route(&self) -> Option<Route> {
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
	pub(super) fn announce(&self, route: Route) -> Result<(), Error> {
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
	pub(super) fn unannounce(&self) {
		self.alive.unannounce();
	}

	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// A watch-only handle to whether anyone reads the broadcast. See [`Demand`].
	pub fn demand(&self) -> Demand {
		Demand::new(self.alive.token.consume().weak(), self.state.clone())
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
		// [`Requests`] queues such requests). Creating the track fulfills that
		// request: its consumers resolve against this very producer. Without
		// this they would be stranded, since the name is taken the moment the
		// track exists, so no handler could ever serve their queue entry.
		if let Some(request) = state.requests.take(name.as_ref()) {
			let track = request.with_stats(self.stats.clone()).accept(info.clone());
			// Cache it like a served request so concurrent lookups coalesce; a
			// live same-name entry cannot exist (its presence would have kept
			// the request from queuing).
			let _ = state.cache_track(track.weak());
			let unlist = Unlist::list(&self.state, &mut state.listing, name, &info);
			return Ok(track.listed(unlist));
		}

		let track = track::Producer::new(self.info.clone(), name.clone(), info.clone()).with_stats(self.stats.clone());
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
	/// [`Requests::poll_requested_track`].
	///
	/// Subscribers wait on the name until it is accepted, so a reservation the producer
	/// ends up never filling has to be dropped or rejected. Ending the broadcast
	/// resolves whatever is left.
	pub fn reserve_track(&self, name: impl Into<Arc<str>>) -> Result<track::Request, Error> {
		let name = name.into();
		let mut state = self.state.lock();
		let request = track::Request::new(self.info.clone(), name.clone()).with_stats(self.stats.clone());
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
	pub fn requests(&self) -> Requests {
		Requests::new(
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
	/// Retracts its announcement and local discovery; a later [`super::origin::Publication::announce`] fails
	/// with [`Error::Closed`]. Tracks already handed out carry on and end with their
	/// own finish or abort. Every later [`Consumer::track`], and every name reserved or
	/// still queued for a handler, answers [`Error::Unroutable`]: the same answer an
	/// origin gives for a path nobody publishes. A request a [`Requests`] already took is
	/// left for that handler to answer.
	///
	/// Dropping the last producer does the same. Closing twice is a no-op.
	pub fn close(&self) {
		self.alive.close();
	}

	/// Whether any [`Consumer`] handle exists, which is not itself demand for a track.
	pub(super) fn is_held(&self) -> bool {
		self.alive.token.is_used()
	}

	/// `Ready(Ok)` once a [`Consumer`] handle exists, `Ready(Err)` once the broadcast ended.
	pub(super) fn poll_held(&self, waiter: &kio::Waiter) -> Poll<Result<(), kio::Closed>> {
		self.alive.token.poll_used(waiter)
	}

	/// `Ready(Ok)` once no [`Consumer`] handle is left, `Ready(Err)` once the broadcast ended.
	pub(crate) fn poll_unheld(&self, waiter: &kio::Waiter) -> Poll<Result<(), kio::Closed>> {
		self.alive.token.poll_unused(waiter)
	}

	/// [`Self::close`], unless a [`Consumer`] holds the broadcast or a track it asked for
	/// still waits for a handler. Atomic with minting a consumer, so a holder either keeps
	/// the broadcast or only ever sees it closed. Returns whether the broadcast is closed.
	pub(crate) fn close_unheld(&self) -> bool {
		self.alive.close_unheld()
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// Ends the broadcast on [`Producer::close`] or when the last [`Producer`] or
/// [`Requests`] drops, closing the liveness channel every [`Consumer`] watches.
///
/// A refcount rather than a "am I the last one?" check inside `Drop`: that answer is
/// a snapshot, and acting on it is exactly what invalidates it.
struct Alive {
	token: kio::Producer<()>,
	state: kio::Shared<BroadcastState>,
	// The advertisement of the broadcast's exact path, owned here so it retracts
	// with the broadcast: on close or the last producer-side handle dropping. `None` for a standalone broadcast.
	announcer: kio::Lock<Option<Announcer>>,
	// How long a session's source still names the instance that minted it. Held on the
	// producer side, never by a consumer, so the selection it may hold is dropped by its
	// session rather than wherever the last consumer goes.
	validity: RequestValidity,
}

impl Alive {
	fn new(state: kio::Shared<BroadcastState>) -> Arc<Self> {
		Arc::new(Self {
			token: kio::Producer::default(),
			state,
			announcer: kio::Lock::new(None),
			validity: RequestValidity::default(),
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
			state.close_listing();
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
			state.close_listing();
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
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) enum RequestValidity {
	#[default]
	Unrestricted,
	Route(kio::Consumer<()>),
	Selected(std::sync::Arc<crate::model::origin_impl::Selection>),
	/// One period of a selection and its answering lease, captured by [`Self::start`].
	Period(
		kio::Consumer<()>,
		kio::Consumer<()>,
		std::sync::Arc<crate::model::origin_impl::Selection>,
	),
}

impl RequestValidity {
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn token(route: Option<kio::Consumer<()>>) -> Self {
		route.map(Self::Route).unwrap_or_default()
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn for_source(route: &crate::origin::Requests, path: &crate::Path) -> Self {
		route
			.request_validity(path)
			.map(|selected| Self::Selected(std::sync::Arc::new(selected)))
			.unwrap_or_else(|| Self::Route(kio::Producer::new(()).consume()))
	}

	/// Capture one selection period so a pending request never revives after a replacement.
	pub(crate) fn start(&self) -> Self {
		match self {
			Self::Selected(selected) => Self::Period(selected.current(), selected.answering(), selected.clone()),
			other => other.clone(),
		}
	}

	pub(crate) fn check(&self, waiter: &kio::Waiter) -> Result<(), crate::Error> {
		match self {
			Self::Route(route) if route.poll_closed(waiter).is_ready() => Err(crate::Error::Unroutable),
			// A replacement retires the period only once its held `Restart` is released.
			Self::Period(route, _, selected)
				if route.poll_closed(waiter).is_ready() || selected.poll_retired(waiter).is_ready() =>
			{
				Err(crate::Error::Unroutable)
			}
			Self::Selected(_) => self.start().check(waiter),
			_ => Ok(()),
		}
	}

	/// [`Self::check`] for a request already sent but not yet answered: only another
	/// instance taking the path refuses it, since without an epoch that instance may be
	/// the one answering. A withdrawal leaves it to its answer, as moq-lite has the
	/// publisher conclude in-flight requests.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn check_unanswered(&self, waiter: &kio::Waiter) -> Result<(), crate::Error> {
		match self {
			Self::Period(_, answering, selected)
				if answering.poll_closed(waiter).is_ready() || selected.poll_retired(waiter).is_ready() =>
			{
				Err(crate::Error::Unroutable)
			}
			Self::Selected(_) => self.start().check_unanswered(waiter),
			_ => Ok(()),
		}
	}
}

#[cfg(test)]
#[expect(missing_docs, reason = "test-only assertion helpers")]
impl Producer {
	pub fn assert_create_track(
		&mut self,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<track::Info>>,
	) -> track::Producer {
		self.create_track(name, info).expect("should not have errored")
	}
}

/// A session-owned source broadcast, closed on drop even while the session's
/// serve machines still hold its [`Requests`].
///
/// A peer's retraction and a dead session end the source the same way: a broadcast
/// carries no end cause past this hop. Shared by the lite and IETF subscribers.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) struct SourceGuard(Producer);

impl SourceGuard {
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
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
pub struct Requests {
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

/// Counts one live [`Requests`], rejecting the queued requests when the last one drops.
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

impl Requests {
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
		// A source that no longer names the instance that minted it takes no new track:
		// ending it refuses what is queued with `Unroutable`, while the tracks it handed
		// out run on. Checked outside the broadcast lock, since the selection reads the origin.
		if self.alive.validity.check(waiter).is_err() {
			self.alive.close();
		}
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
		let _ = state.cache_track(pending.weak());
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

	/// What a request to the peer for this source must still hold when it goes out, for
	/// an operation on a track the source already handed out.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn validity(&self) -> &RequestValidity {
		&self.alive.validity
	}

	/// Answer the broadcast's track listings, for a handler that knows the tracks it
	/// would serve, such as a relay listing its upstream's. See [`Lister`].
	///
	/// A handler that serves any name on request takes none, so it lists nothing. One
	/// answer per broadcast: fails with [`Error::Duplicate`] while another lister exists.
	pub(crate) fn lister(&self) -> Result<Lister, Error> {
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
#[expect(missing_docs, reason = "test-only assertion helpers")]
impl Requests {
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
	pub(super) fn with_path(mut self, path: crate::Path) -> Self {
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
	/// [`Requests`] would serve a request for it.
	pub fn track(&self, name: &str) -> Result<track::Consumer, Error> {
		let current = {
			let mut state = self.state.lock();
			// An ended broadcast serves nothing new, not even a track it still has:
			// the lookup answers what a fresh `request` for the path would.
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

	/// Join the demand of the track serving `name`, requesting it from a [`Requests`]
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
		let request = track::Request::new(self.info.clone(), name.clone());
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

/// A cloneable, watch-only handle to whether anyone reads a broadcast.
///
/// Obtained from [`Producer::demand`]. The broadcast is wanted while any of its tracks is
/// ([`track::Demand::wanted`] is `Some`) or a track request waits for a [`Requests`]
/// handler, so a publisher can follow its readers without holding every track's producer.
/// It's a weak handle: it neither keeps the broadcast alive nor counts as demand.
/// Each clone watches on its own, so its first
/// [`poll_wanted_changed`](Self::poll_wanted_changed) reports the current value.
pub struct Demand {
	alive: kio::ConsumerWeak<()>,
	state: kio::Shared<BroadcastState>,
	// This handle's inbox in [`BroadcastState::demands`].
	inbox: usize,
	// One watcher per track, each counting its track in `wanted` while the track is wanted,
	// so a reader coming or going re-polls only its own track.
	watchers: kio::Tasks<Watcher>,
	wanted: Arc<AtomicUsize>,
	// The value `poll_wanted_changed` last returned, or `None` before it returned any.
	prev: Option<bool>,
}

impl Clone for Demand {
	fn clone(&self) -> Self {
		Self::new(self.alive.clone(), self.state.clone())
	}
}

impl Demand {
	fn new(alive: kio::ConsumerWeak<()>, state: kio::Shared<BroadcastState>) -> Self {
		let inbox = {
			let mut state = state.lock();
			let open = state
				.tracks
				.iter()
				.filter(|track| !track.is_closed())
				.cloned()
				.collect();
			state.demands.insert(open)
		};
		Self {
			alive,
			state,
			inbox,
			watchers: kio::Tasks::new(),
			wanted: Arc::default(),
			prev: None,
		}
	}

	/// Whether the broadcast is open and any of its tracks is wanted or requested right now.
	pub fn wanted(&self) -> bool {
		if self.alive.is_closed() {
			return false;
		}
		let state = self.state.read();
		!state.requests.is_empty() || state.tracks.iter().any(|track| track.is_used())
	}

	/// Return [`wanted`](Self::wanted) once it differs from what this handle last returned:
	/// immediately on the first call, then on each change. Errors once the broadcast closes.
	///
	/// So `while !demand.wanted_changed().await? {}` waits for a reader, and
	/// `while demand.wanted_changed().await? {}` waits for the last one to leave.
	#[cfg(feature = "async")]
	pub async fn wanted_changed(&mut self) -> Result<bool, Error> {
		kio::wait(|waiter| self.poll_wanted_changed(waiter)).await
	}

	/// Poll for [`wanted`](Self::wanted) to differ from what this handle last returned.
	pub fn poll_wanted_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<bool, Error>> {
		if self.alive.poll_closed(waiter).is_ready() {
			return Poll::Ready(Err(Error::Dropped));
		}
		let requested = self.poll_cached(waiter);
		// Ready only while no track is watched; the parent waker is registered either way.
		let _ = self.watchers.poll(waiter);
		let wanted = requested || self.wanted.load(Ordering::Relaxed) > 0;
		if self.prev == Some(wanted) {
			return Poll::Pending;
		}
		self.prev = Some(wanted);
		Poll::Ready(Ok(wanted))
	}

	/// Watch the tracks cached since the last look, returning whether a track request is
	/// pending. Parks `waiter` on either changing.
	fn poll_cached(&mut self, waiter: &kio::Waiter) -> bool {
		loop {
			let mut requested = false;
			let polled = self.state.poll(waiter, |state| {
				requested = !state.requests.is_empty();
				match state.demands[self.inbox].is_empty() {
					true => Poll::Pending,
					false => Poll::Ready(()),
				}
			});
			let Poll::Ready(mut state) = polled else {
				return requested;
			};
			let cached = std::mem::take(&mut state.demands[self.inbox]);
			drop(state);
			for track in cached {
				self.watchers.push(Watcher {
					track: track.demand(),
					counted: false,
					wanted: self.wanted.clone(),
				});
			}
		}
	}
}

impl Drop for Demand {
	fn drop(&mut self) {
		self.state.lock().demands.remove(self.inbox);
	}
}

/// Counts one track in its [`Demand`] while the track is wanted.
struct Watcher {
	track: track::Demand,
	counted: bool,
	wanted: Arc<AtomicUsize>,
}

impl kio::Task for Watcher {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let state = self.track.poll_state(waiter);
		let wanted = state == track::DemandState::Active;
		if wanted != self.counted {
			self.counted = wanted;
			match wanted {
				true => self.wanted.fetch_add(1, Ordering::Relaxed),
				false => self.wanted.fetch_sub(1, Ordering::Relaxed),
			};
		}
		match state {
			track::DemandState::Closed => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}
}

#[cfg(test)]
#[expect(missing_docs, reason = "test-only assertion helpers")]
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

	#[test]
	fn a_subscription_is_registered_before_its_request_is_visible() {
		struct Serve {
			dynamic: std::sync::Mutex<Requests>,
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
			dynamic: std::sync::Mutex::new(producer.requests()),
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

	/// Records whether it was woken.
	#[derive(Default)]
	struct Flag(std::sync::atomic::AtomicBool);

	impl std::task::Wake for Flag {
		fn wake(self: Arc<Self>) {
			self.0.store(true, Ordering::Relaxed);
		}
	}

	impl Flag {
		fn waiter(self: &Arc<Self>) -> kio::Waiter {
			kio::Waiter::new(self.clone().into())
		}

		fn take(&self) -> bool {
			self.0.swap(false, Ordering::Relaxed)
		}
	}

	/// Poll `demand` for a change with `waiter`, without waiting.
	fn wanted_changed(demand: &mut Demand, waiter: &kio::Waiter) -> Option<bool> {
		match demand.poll_wanted_changed(waiter) {
			Poll::Ready(res) => Some(res.unwrap()),
			Poll::Pending => None,
		}
	}

	/// Any track's demand is the broadcast's, whether the track existed before the handle or
	/// came after it, and a reader arriving straight through a track producer, which never
	/// touches the broadcast's state, still wakes it. It stays wanted until the last wanted
	/// track goes.
	#[test]
	fn demand_follows_every_track() {
		let producer = Info::new().produce();
		let video = producer.create_track("video", None).unwrap();
		let mut demand = producer.demand();
		let audio = producer.create_track("audio", None).unwrap();
		let flag = Arc::new(Flag::default());
		let waiter = flag.waiter();
		assert_eq!(
			wanted_changed(&mut demand, &waiter),
			Some(false),
			"the first call reports"
		);

		let _bare = producer.consume().track("audio").unwrap();
		assert_eq!(
			wanted_changed(&mut demand, &waiter),
			None,
			"a bare handle is not demand"
		);

		flag.take();
		let reading_audio = audio.subscribe(None);
		assert!(flag.take(), "a reader wakes the parked poll");
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(true));
		assert!(demand.wanted());

		let reading_video = video.subscribe(None);
		assert_eq!(wanted_changed(&mut demand, &waiter), None, "already wanted");
		drop(reading_audio);
		assert_eq!(wanted_changed(&mut demand, &waiter), None, "video is still read");
		flag.take();
		drop(reading_video);
		assert!(flag.take(), "the last reader leaving wakes the parked poll");
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(false));
		assert!(!demand.wanted());

		// A clone reports the current value first, whatever the original last returned.
		let _reading = video.subscribe(None);
		assert_eq!(wanted_changed(&mut demand.clone(), &waiter), Some(true));
	}

	/// A request waiting for a handler is demand, and handing it out moves that demand onto
	/// the track it became without a gap, until its subscriber leaves.
	#[test]
	fn a_pending_request_is_demand_until_its_reader_leaves() {
		let producer = Info::new().produce();
		let mut requests = producer.requests();
		let mut demand = producer.demand();
		let waiter = kio::Waiter::noop();
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(false));

		let subscribing = producer.consume().track("video").unwrap().subscribe(None);
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(true), "queued");

		let Poll::Ready(Ok(request)) = requests.poll_requested_track(&waiter) else {
			panic!("the request was not queued");
		};
		assert_eq!(wanted_changed(&mut demand, &waiter), None, "handed out, still read");
		let _track = request.accept(None);
		assert_eq!(wanted_changed(&mut demand, &waiter), None, "accepted, still read");

		drop(subscribing);
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(false));
	}

	/// A closed track is never demand, even while a reader lingers to drain it.
	#[test]
	fn a_closed_track_is_not_demand() {
		let producer = Info::new().produce();
		let video = producer.create_track("video", None).unwrap();
		let mut demand = producer.demand();
		let _reading = video.subscribe(None);
		let waiter = kio::Waiter::noop();
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(true));

		video.abort(Error::Cancel).unwrap();
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(false));
		assert!(!demand.wanted());
	}

	/// The handle reports the broadcast closing, wanted or not.
	#[test]
	fn demand_errors_once_closed() {
		let producer = Info::new().produce();
		let video = producer.create_track("video", None).unwrap();
		let _reading = video.subscribe(None);
		let mut demand = producer.demand();
		producer.close();
		assert!(!demand.wanted());
		assert!(matches!(
			demand.poll_wanted_changed(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Dropped))
		));
	}

	/// A reader of a frame keeps its group wanted, the group its track, and the track the
	/// broadcast, after every handle above it is gone.
	#[test]
	fn a_frame_read_keeps_the_broadcast_wanted() {
		let producer = Info::new().produce();
		let video = producer.create_track("video", track::Info::timed()).unwrap();
		let mut demand = producer.demand();
		let waiter = kio::Waiter::noop();
		let mut subscriber = video.subscribe(None);
		let mut writing = video.append_group().unwrap();
		writing.write_frame(crate::Timestamp::ZERO, &b"key"[..]).unwrap();
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(true));

		let mut group = subscriber.assert_group();
		drop(subscriber);
		assert_eq!(wanted_changed(&mut demand, &waiter), None, "the group is read");
		assert!(video.demand().wanted().is_some());

		let Poll::Ready(Ok(Some(frame))) = group.poll_next_frame(&waiter) else {
			panic!("the frame was written");
		};
		drop(group);
		assert!(writing.demand().wanted(), "the frame's reader reads the group");
		assert_eq!(wanted_changed(&mut demand, &waiter), None, "the frame is read");

		drop(frame);
		assert!(!writing.demand().wanted());
		assert_eq!(video.demand().wanted(), None);
		assert_eq!(wanted_changed(&mut demand, &waiter), Some(false));
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
		let dynamic = producer.requests();

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
		let mut producer = Info::new().produce().requests();

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
		let mut broadcast = Info::new().produce().requests();
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

	/// A broadcast keeps nothing of a name once its track is gone: a replacement numbers and
	/// times its groups from scratch, and the application owns keeping them past its
	/// predecessor's.
	#[test]
	fn replacement_starts_over() {
		let at = |millis| crate::Timestamp::from_millis(millis).unwrap();
		let producer = Info::new().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();

		let track = consumer.track("media").unwrap().subscribe(None);
		let first = dynamic.assert_request().accept(crate::track::Info::timed());
		let mut group = first.create_group(crate::group::Info { sequence: 8 }).unwrap();
		group.write_frame(at(1000), b"a".as_slice()).unwrap();
		group.finish().unwrap();
		first.finish().unwrap();
		drop((first, track));

		let track = consumer.track("media").unwrap().subscribe(None);
		let second = dynamic.assert_request().accept(crate::track::Info::timed());
		let mut group = second.append_group().unwrap();
		assert_eq!(group.sequence, 0);
		group.write_frame(at(500), b"b".as_slice()).unwrap();
		drop((group, second, track));

		let mut third = producer.create_track("media", crate::track::Info::timed()).unwrap();
		third.finish_at(crate::track::Position::group(0)).unwrap();
	}

	/// Names that are gone leave nothing behind, served or not, so churning distinct names
	/// keeps the broadcast bounded by its live tracks.
	#[test]
	fn gone_names_are_forgotten() {
		let producer = Info::new().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		for i in 0..1000 {
			let track = consumer.track(&format!("requested/{i}")).unwrap().subscribe(None);
			let served = dynamic.assert_request().accept(crate::track::Info::timed());
			served.append_group().unwrap().finish().unwrap();
			drop((served, track));

			let track = consumer.track(&format!("refused/{i}")).unwrap().subscribe(None);
			drop((dynamic.assert_request(), track));

			let created = producer
				.create_track(format!("created/{i}"), crate::track::Info::timed())
				.unwrap();
			created.append_group().unwrap().finish().unwrap();
		}
		let state = producer.state.lock();
		assert!(state.tracks.len() < 16, "{} names retained", state.tracks.len());
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn requested_unused() {
		let mut broadcast = Info::new().produce().requests();
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
		let mut dynamic = producer.requests();
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

	// Cloning a `Requests` and dropping the clone must not flip the handler
	// count to zero. The relay's lite subscriber clones the
	// dynamic per spawned subscribe; if Clone skipped the increment, the
	// first finished subscribe would tear down the broadcast and any
	// follow-up `track` would return `NotFound`.
	#[test]
	fn dynamic_clone_keeps_alive() {
		let broadcast = Info::new().produce().requests();
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
		let mut dynamic = producer.requests();
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
		let dynamic = producer.requests();
		let consumer = dynamic.consume();

		let pending = subscribe_pending!(consumer, "track1");

		producer.close();
		assert!(matches!(pending.await, Err(Error::Unroutable)));
		drop(dynamic);
	}

	/// A queued request answers the same when the broadcast ends by its last handle
	/// dropping, even when that handle is the `Requests` that would have served it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_the_last_handle_resolves_a_queued_request() {
		let dynamic = Info::new().produce().requests();
		let consumer = dynamic.consume();

		let pending = subscribe_pending!(consumer, "track1");

		drop(dynamic);
		assert!(matches!(pending.await, Err(Error::Unroutable)));
	}

	/// A source that no longer names the instance that minted it takes no new track, even
	/// from a handler that checks nothing: its request stream ends and every later request
	/// answers `Unroutable`, while the track it accepted earlier runs on.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_lapsed_source_refuses_new_tracks() {
		let route = kio::Producer::new(());
		let producer = Info::new()
			.produce()
			.with_validity(RequestValidity::Route(route.consume()));
		let mut dynamic = producer.requests();
		let consumer = producer.consume();

		let pending = subscribe_pending!(consumer, "accepted");
		let served = dynamic.assert_request().accept(crate::track::Info::timed());
		let mut accepted = pending.await.unwrap();
		let queued = subscribe_pending!(consumer, "queued");

		drop(route);
		assert!(matches!(dynamic.requested_track().await, Err(Error::Closed)));
		assert!(matches!(queued.await, Err(Error::Unroutable)));
		assert!(matches!(consumer.track("later"), Err(Error::Unroutable)));

		served.append_group().unwrap().finish().unwrap();
		accepted.assert_group();
	}

	/// With a producer still alive, losing the last handler is not the broadcast ending:
	/// the queued request fails as `Dropped`.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn dropping_the_last_handler_resolves_a_queued_request_dropped() {
		let producer = Info::new().produce();
		let dynamic = producer.requests();
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
		let mut dynamic = producer.requests();
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
		let dynamic = request.requests();
		let track = consumer.track("track1").unwrap();
		let pending_fetch = track.fetch_group(0, None);
		let fetch = dynamic.requested_fill_group().await.unwrap();
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
}
