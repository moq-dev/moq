//! A broadcast is a named collection of tracks, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] creates tracks on demand: a [Consumer] subscribes by name, and the
//! producer either serves a track it already has or is handed a [`track::Request`] to
//! fill. Both handles are refcounted clones of one broadcast, which closes when the
//! last producer drops.
//!
//! [Info] is the static metadata; [Route] is the dynamic path the broadcast takes to
//! reach an origin, including whether it is announced to subscribers.
use crate::track;
use std::{
	collections::{HashMap, VecDeque},
	sync::Arc,
	task::{Poll, ready},
};

use crate::Error;

use super::{OriginList, Requests, WeakCache};

/// A collection of media tracks that can be published and subscribed to.
///
/// Create via [`Info::produce`] to obtain both [`Producer`] and [`Consumer`] pair.
/// This is the broadcast's static identity, fixed for its lifetime; the path it
/// takes to get here is the dynamic [`Route`], observed via [`Consumer::route`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Info {
	/// The origin this broadcast belongs to (its identity, and the cache pool its
	/// tracks and groups inherit). A track reaches its pool by walking up this link,
	/// so the pool has a single home on the origin rather than being copied per
	/// broadcast. Defaults to an unknown origin with an unbounded pool (a standalone
	/// broadcast with no relay origin).
	pub origin: super::origin::Info,
}

impl Info {
	/// Create a new broadcast with default metadata.
	pub fn new() -> Self {
		Self::default()
	}

	/// Consume this [Info] to create a producer that carries its metadata.
	///
	/// Keep the returned [`Producer`] alive for as long as the broadcast should stay
	/// available, and end it with [`Producer::finish`]. See the note on [`Producer`].
	pub fn produce(self) -> Producer {
		Producer::new(self)
	}
}

/// The path a broadcast takes to reach this origin, and how preferable it is.
///
/// Unlike [`Info`], the route is dynamic: it changes when the serving session fails
/// over, the upstream topology shifts, or the publisher re-advertises itself.
/// Publish a change with [`Producer::set_route`] and observe one with
/// [`Consumer::route_changed`]; downstream sessions forward updates as a restart
/// on the wire, so route churn never looks like a new broadcast.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
	/// The chain of origins the broadcast has traversed, oldest first. Each relay
	/// appends its own [`crate::Origin`] when forwarding; used for loop detection
	/// and as the selection tie-break.
	pub hops: OriginList,

	/// The cost of pulling the broadcast via this route, accumulated per link:
	/// lower wins, with ties broken by hop length and then a deterministic hash.
	///
	/// The original publisher seeds it with its production cost (zero for a live
	/// publish, something large for a standby that would have to start working,
	/// like a cold transcoder), and each link adds its own configured price as
	/// the announcement crosses it, so a route over a metered backbone ranks
	/// worse than an equal-length one within a datacenter. The accumulation
	/// restarts at zero at any node actively carrying the broadcast: those
	/// upstream legs already exist and are not re-paid by one more subscriber,
	/// so the sum is the cost of the transfers a subscription would newly cause.
	///
	/// Carried on the wire from lite-06; older peers always report zero, leaving
	/// the hop-count tie-break as the effective metric exactly as before.
	pub cost: u64,

	/// The cost as the announcing peer advertised it, before this link's charge
	/// was added to [`Self::cost`]. Local bookkeeping, never forwarded: zero on a
	/// chain of two or more hops means the announcing relay is actively carrying
	/// the broadcast, which is what the origin's handover gate keys on.
	pub(crate) advertised: u64,

	/// Whether the broadcast should be announced: advertised to consumers via
	/// [`crate::origin::Consumer::announced`] while this is the best route. A
	/// non-announced broadcast stays reachable by exact path for subscribes and
	/// fetches (e.g. serving cached or on-demand content), so toggling this via
	/// [`Producer::set_route`] announces or unannounces without touching the
	/// broadcast itself. Defaults to `false`.
	pub announce: bool,
}

impl Route {
	/// An unannounced direct route: no hops, best cost.
	///
	/// The broadcast is reachable only by its exact path, so subscribers must already
	/// know it exists. Use [`announced`](Self::announced) to advertise it instead.
	pub fn new() -> Self {
		Self::default()
	}

	/// An announced direct route: no hops, best cost.
	///
	/// The broadcast is advertised to subscribers via
	/// [`crate::origin::Consumer::announced`] while this is the best route, on top of
	/// staying reachable by exact path. Use [`new`](Self::new) to keep it unadvertised.
	pub fn announced() -> Self {
		Self {
			announce: true,
			..Self::default()
		}
	}

	/// Append a hop to the chain, oldest first.
	///
	/// Fails with [`crate::TooManyOrigins`] once the chain is full, the same limit
	/// the wire enforces.
	pub fn with_hop(mut self, origin: super::Origin) -> Result<Self, super::TooManyOrigins> {
		self.hops.push(origin)?;
		Ok(self)
	}

	/// Replace the hop chain.
	pub fn with_hops(mut self, hops: OriginList) -> Self {
		self.hops = hops;
		self
	}

	/// Set the cost: lower wins among routes serving the same broadcast.
	pub fn with_cost(mut self, cost: u64) -> Self {
		self.cost = cost;
		self
	}

	/// Set whether the broadcast is announced via this route.
	pub fn with_announce(mut self, announce: bool) -> Self {
		self.announce = announce;
		self
	}
}

#[derive(Default)]
struct BroadcastState {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	// Keyed by the track's shared `Arc<str>` name (the same Arc the handle holds).
	// The cache reclaims closed entries incrementally on insert so a long-lived
	// broadcast churning distinct track names stays bounded by the live count.
	tracks: WeakCache<Arc<str>, track::TrackWeak>,

	// Pending requests keyed by track name, coalescing concurrent `track()` calls
	// and waiting for a dynamic handler to accept or deny them. A request leaves
	// here once handed out (the handler caches it in `tracks`, so lookups keep
	// coalescing onto it there).
	requests: Requests<Arc<str>, track::Request>,

	// Route-fed mode (a relay/origin "front"): tracks are spliced logical tracks
	// joined across per-session tracks. `None` for an ordinary broadcast.
	spliced: Option<SplicedState>,

	// The path the broadcast currently takes to reach us, bumping `route_epoch`
	// on every change so consumers can watch for updates.
	route: Route,
	route_epoch: u64,

	// Set by an explicit `Producer::finish()` or `Producer::abort()` so `Drop` can
	// tell a deliberate shutdown apart from a producer dropped by accident.
	closing: bool,
}

/// The spliced (route-fed) half of a broadcast: logical tracks that outlive any
/// single session, plus the queue of tracks awaiting a serving route.
#[derive(Default)]
struct SplicedState {
	// Logical tracks by name, owned strongly: they live as long as the broadcast
	// (the origin's front), not as long as any consumer.
	tracks: HashMap<Arc<str>, super::resume::Producer>,

	// Names awaiting assignment to a route, in request order.
	pending: VecDeque<Arc<str>>,
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

	/// Live demand: a subscribed spliced track (route-fed broadcast), or a
	/// pending request / consumed track (ordinary broadcast). See [`Demand`].
	fn is_used(&self) -> bool {
		if let Some(spliced) = &self.spliced {
			return spliced.tracks.values().any(|track| track.is_used());
		}
		!self.requests.is_empty() || self.tracks.iter().any(|track| track.is_used())
	}

	/// Park `waiter` on every per-track channel feeding [`Self::is_used`]: the
	/// consumer counts live on those channels, and their flips don't write this
	/// state, so a watcher registered here alone would miss the edge. `want`
	/// picks the direction; each channel only arms while its side is unmet.
	fn register_demand(&self, waiter: &kio::Waiter, want: bool) {
		if let Some(spliced) = &self.spliced {
			for track in spliced.tracks.values() {
				match want {
					true => track.poll_used(waiter),
					false => track.poll_unused(waiter),
				}
			}
			return;
		}
		for track in self.tracks.iter() {
			match want {
				true => track.poll_used(waiter),
				false => track.poll_unused(waiter),
			}
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
/// **You must keep this producer alive for as long as the broadcast should stay
/// available.** A broadcast lives as long as at least one [`Producer`] exists;
/// children do *not* keep it alive (cloning a [`Consumer`] or holding a
/// [`track::Producer`] does nothing for the broadcast's lifetime). When the last
/// producer goes away every consumer observes [`Error::Dropped`].
///
/// End the broadcast with [`Self::finish`] rather than dropping it. Dropping is an
/// easy footgun in garbage-collected bindings (Go, Python, ...), where the handle
/// can be collected the moment it falls out of scope even while you are still
/// publishing, tearing the stream down mid-broadcast. Dropping the last producer
/// without [`Self::finish`] logs a warning.
#[derive(Clone)]
pub struct Producer {
	// Held behind an Arc so each track born from this broadcast can inherit a shared
	// handle (threaded down by [`Self::create_track`] / [`Self::reserve_track`]).
	info: Arc<Info>,

	// Broadcast liveness. Consumers watch this (read-only) for close; dropping every
	// producer (this handle and every `Dynamic`) ends the broadcast.
	alive: kio::Producer<()>,

	// Track registry plus the dynamic request queue, mutated by producers and
	// consumers alike under one lock.
	state: kio::Shared<BroadcastState>,
}

impl Producer {
	/// Create a producer for the given broadcast metadata. Prefer [`Info::produce`].
	pub fn new(info: Info) -> Self {
		Self {
			info: Arc::new(info),
			alive: Default::default(),
			state: Default::default(),
		}
	}

	/// Create a route-fed (spliced) broadcast: consumer track lookups mint logical
	/// tracks that are spliced across per-session tracks, queued for a route to
	/// serve. Used by the origin for broadcasts reached over the network.
	pub(crate) fn new_spliced(info: Info) -> Self {
		Self {
			info: Arc::new(info),
			alive: Default::default(),
			state: kio::Shared::new(BroadcastState {
				spliced: Some(SplicedState::default()),
				..Default::default()
			}),
		}
	}

	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// A watch-only handle to the broadcast's demand. See [`Demand`].
	pub fn demand(&self) -> Demand {
		Demand {
			alive: self.alive.consume().weak(),
			state: self.state.clone(),
		}
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> Result<(), Error> {
		self.state.lock().tracks.remove(name).ok_or(Error::NotFound)?;
		Ok(())
	}

	/// Produce a new track and insert it into the broadcast.
	///
	/// Pass a name and an optional [`track::Info`], so a bare name works:
	/// `create_track("video", None)`.
	pub fn create_track(
		&mut self,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<track::Info>>,
	) -> Result<track::Producer, Error> {
		let info = info.into().unwrap_or_default();
		let track = track::Producer::new(self.info.clone(), name, info);
		self.state.lock().insert_track(track.weak())?;
		Ok(track)
	}

	/// Reserve a track by name without finalizing its [`track::Info`].
	///
	/// Returns a [`track::Request`] already discoverable by consumers; call
	/// [`track::Request::accept`] to set its info and start producing. Use this when
	/// the producer can't pick the track's properties (e.g. timescale) until it has
	/// inspected the media, the same shape as a consumer-driven
	/// [`Dynamic::requested_track`].
	pub fn reserve_track(&mut self, name: impl Into<Arc<str>>) -> Result<track::Request, Error> {
		let request = track::Request::new(self.info.clone(), name);
		self.state.lock().insert_track(request.weak())?;
		Ok(request)
	}

	/// Create a track with a unique name using the given suffix.
	///
	/// Generates names like `0{suffix}`, `1{suffix}`, etc. and picks the first
	/// one not already used in this broadcast.
	pub fn unique_track(
		&mut self,
		suffix: &str,
		info: impl Into<Option<track::Info>>,
	) -> Result<track::Producer, Error> {
		let name = self.unique_name(suffix);
		self.create_track(name, info)
	}

	/// Generate a unique track name from a suffix without creating the track.
	///
	/// Returns a fresh name like `0{suffix}`, `1{suffix}`, etc. Use this when
	/// you need to set non-default Track properties (e.g. `with_timescale`,
	/// `with_latency_max`) before handing the Track to [`Self::create_track`].
	pub fn unique_name(&self, suffix: &str) -> String {
		let state = self.state.read();
		(0u16..)
			.map(|i| format!("{i}{suffix}"))
			.find(|name| !state.tracks.contains_key(name.as_str()))
			.expect("u16 namespace exhausted; wow")
	}

	/// Create a dynamic producer that handles on-demand track requests from consumers.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(self.info.clone(), self.alive.clone(), self.state.clone())
	}

	/// Set the broadcast's [`Route`]: the hop chain and cost it advertises.
	///
	/// Call this when the path to the content changes (an upstream failover) or the
	/// publisher's preference changes (e.g. a transcoder warming up lowers its
	/// cost). Consumers observe the change via [`Consumer::route_changed`] and
	/// sessions forward it downstream as a restart, never as a new broadcast.
	/// Setting the current route again is a no-op.
	pub fn set_route(&mut self, route: Route) -> Result<(), Error> {
		let mut state = self.state.lock();
		if state.route == route {
			return Ok(());
		}
		state.route = route;
		state.route_epoch += 1;
		Ok(())
	}

	/// Poll for the next spliced track awaiting a serving route, returning its name
	/// and logical producer. Route-fed broadcasts only.
	pub(crate) fn poll_spliced_assigned(&self, waiter: &kio::Waiter) -> Poll<(Arc<str>, super::resume::Producer)> {
		let mut state = ready!(self.state.poll(waiter, |state| {
			match &state.spliced {
				Some(spliced) if !spliced.pending.is_empty() => Poll::Ready(()),
				_ => Poll::Pending,
			}
		}));

		let spliced = state.spliced.as_mut().expect("predicate guaranteed spliced");
		let name = spliced.pending.pop_front().expect("predicate guaranteed a request");
		let producer = spliced.tracks.get(&name).expect("pending name without a track").clone();
		Poll::Ready((name, producer))
	}

	/// Abort every spliced track, releasing their subscribers with `err`. Called
	/// when the broadcast closes for good.
	pub(crate) fn abort_spliced(&self, err: Error) {
		let mut state = self.state.lock();
		if let Some(spliced) = state.spliced.as_mut() {
			spliced.pending.clear();
			for producer in spliced.tracks.values_mut() {
				let _ = producer.abort(err.clone());
			}
		}
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info.clone(),
			alive: self.alive.consume(),
			state: self.state.clone(),
			route_seen: None,
		}
	}

	/// Cleanly finish the broadcast once you are done publishing.
	///
	/// Marks the broadcast as deliberately finished so consumers observe a normal
	/// end. Prefer this over dropping the producer: an accidental drop (see the note
	/// on [`Producer`]) logs a warning, whereas `finish()` is silent.
	///
	/// Only marks intent; the broadcast actually ends once every producer clone is
	/// gone, so a clone that outlives this call keeps it alive until it too is
	/// dropped or finished.
	pub fn finish(self) {
		self.state.lock().closing = true;
	}

	/// Mark the broadcast as deliberately ended, without the
	/// dropped-without-finish warning. Same effect as [`Self::finish`], but takes
	/// `&self` for callers that can't consume the producer. Used by sessions
	/// tearing down announced broadcasts when the connection dies.
	pub(crate) fn abort(&self) {
		self.state.lock().closing = true;
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		// Only the last producer ending the broadcast matters; a clone dropping
		// leaves it live (`alive` is shared with every `Dynamic` too). Warn if that
		// last exit wasn't an explicit finish(), since consumers will then see
		// Error::Dropped (classically a GC-collected handle in a language binding
		// that tears the stream down mid-publish).
		if !self.alive.is_last() {
			return;
		}
		if !self.state.read().closing {
			tracing::warn!(
				"broadcast::Producer dropped without finish(). Keep the producer alive while publishing, then call finish()."
			);
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
/// [`crate::origin::Producer::create_broadcast`]: [`Self::finish`] ends it
/// deliberately, while dropping the guard marks the source aborted (keeping the
/// dropped-without-finish warning quiet). Either way the origin unannounces once
/// the last source detaches. Shared by the lite and IETF subscribers so the
/// drop-vs-finish contract lives in one place.
pub(crate) struct SourceGuard {
	// `Option` so `finish` can consume the producer while `Drop` aborts it.
	producer: Option<Producer>,
}

impl SourceGuard {
	pub fn new(producer: Producer) -> Self {
		Self {
			producer: Some(producer),
		}
	}

	/// A clone of the guarded producer.
	pub fn producer(&self) -> Producer {
		self.producer.clone().expect("guard holds a producer until finished")
	}

	/// End the source deliberately: the origin detaches it immediately,
	/// unannouncing the path if it was the last.
	pub fn finish(mut self) {
		if let Some(producer) = self.producer.take() {
			producer.finish();
		}
	}

	/// Update the source's advertised route in place.
	pub fn set_route(&mut self, route: Route) {
		if let Some(producer) = &mut self.producer {
			let _ = producer.set_route(route);
		}
	}
}

impl Drop for SourceGuard {
	fn drop(&mut self) {
		if let Some(producer) = &self.producer {
			producer.abort();
		}
	}
}

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, the dynamic producer
/// picks up the request via [`Self::requested_track`] and either
/// [`track::Request::accept`]s it with a concrete [`track::Info`] or
/// [`track::Request::reject`]s it. Dropped when no longer needed; pending requests
/// are automatically aborted.
pub struct Dynamic {
	info: Arc<Info>,
	// Keeps the broadcast alive while a handler exists (mirrors a producer).
	alive: kio::Producer<()>,
	state: kio::Shared<BroadcastState>,
}

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: count each live handle. Without this, deriving Clone would
		// let `Drop` decrement past `new`'s single increment and prematurely flip
		// the handler count to zero, causing future `track` calls to return `NotFound`.
		self.state.lock().requests.add_handler();

		Self {
			info: self.info.clone(),
			alive: self.alive.clone(),
			state: self.state.clone(),
		}
	}
}

impl Dynamic {
	fn new(info: Arc<Info>, alive: kio::Producer<()>, state: kio::Shared<BroadcastState>) -> Self {
		state.lock().requests.add_handler();

		Self { info, alive, state }
	}

	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// Poll for the next consumer-requested track, without blocking.
	///
	/// Returns [`Error::Closed`] once the broadcast was deliberately ended
	/// ([`Producer::finish`] or aborted), so a serving loop knows to stop and
	/// release its handle.
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
		Poll::Ready(Ok(pending))
	}

	/// Block until a consumer requests a track, returning a [`track::Request`] to serve.
	pub async fn requested_track(&mut self) -> Result<track::Request, Error> {
		kio::wait(|waiter| self.poll_requested_track(waiter)).await
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info.clone(),
			alive: self.alive.consume(),
			state: self.state.clone(),
			route_seen: None,
		}
	}

	/// Block until the broadcast is closed (every producer dropped), returning the cause.
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the broadcast closes; ready with the cause (always [`Error::Dropped`],
	/// since a broadcast only ends by every producer dropping).
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.alive.poll_closed(waiter).map(|()| Error::Dropped)
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for Dynamic {
	fn drop(&mut self) {
		// Decrement and reject under one lock, so a `track` call that saw a live
		// handler through the same lock can't slip a request past the rejection.
		let mut state = self.state.lock();
		if state.requests.remove_handler() {
			// No handlers left to fulfill pending requests; reject them so consumers
			// don't block forever on tracks nobody will serve.
			for request in state.requests.drain_queued() {
				request.reject(Error::Dropped);
			}
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl Dynamic {
	pub fn assert_request(&mut self) -> track::Request {
		self.requested_track()
			.now_or_never()
			.expect("should not have blocked")
			.expect("should not have errored")
	}

	pub fn assert_no_request(&mut self) {
		assert!(self.requested_track().now_or_never().is_none(), "should have blocked");
	}
}

/// Subscribe to arbitrary broadcast/tracks.
pub struct Consumer {
	info: Arc<Info>,
	// Broadcast liveness (read-only): watched for close.
	alive: kio::Consumer<()>,
	// Track registry plus request queue; `track()` reads the registry and enqueues requests.
	state: kio::Shared<BroadcastState>,
	// The route epoch last yielded by `route_changed`, so each consumer clone
	// observes the current route first and every change after it exactly once.
	route_seen: Option<u64>,
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			alive: self.alive.clone(),
			state: self.state.clone(),
			// Reset the cursor so the clone observes the current route first,
			// even if the original already drained `route_changed`.
			route_seen: None,
		}
	}
}

impl Consumer {
	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// The [`Route`] the broadcast currently takes to reach this origin.
	pub fn route(&self) -> Route {
		self.state.read().route.clone()
	}

	/// Poll for a route change. See [`Self::route_changed`].
	pub fn poll_route_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Route, Error>> {
		let seen = self.route_seen;
		if let Poll::Ready(state) = self.state.poll(waiter, |state| {
			if seen != Some(state.route_epoch) {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}) {
			self.route_seen = Some(state.route_epoch);
			return Poll::Ready(Ok(state.route.clone()));
		}
		// No pending change: surface the broadcast's end instead of parking forever.
		ready!(self.alive.poll_closed(waiter));
		Poll::Ready(Err(Error::Dropped))
	}

	/// Wait for the broadcast's [`Route`] to change.
	///
	/// The first call returns the current route immediately; each later call blocks
	/// until it changes again, so a loop observes the initial value followed by
	/// every update. Returns [`Error::Dropped`] once every producer is gone.
	pub async fn route_changed(&mut self) -> Result<Route, Error> {
		kio::wait(|waiter| self.poll_route_changed(waiter)).await
	}

	/// Get a handle to a track on this broadcast.
	pub fn track(&self, name: &str) -> Result<track::Consumer, Error> {
		// A closed broadcast (every producer and handler gone) serves nothing.
		if self.is_closed() {
			return Err(Error::Dropped);
		}

		let mut state = self.state.lock();

		// A route-fed broadcast mints spliced logical tracks: they outlive any
		// session, and a route is asked (via the pending queue) to start serving.
		if let Some(spliced) = state.spliced.as_mut() {
			if let Some(producer) = spliced.tracks.get(name) {
				return Ok(track::Consumer::spliced(name.into(), producer.consume()));
			}
			let name: Arc<str> = name.into();
			let producer = super::resume::Producer::new();
			let consumer = producer.consume();
			spliced.tracks.insert(name.clone(), producer);
			spliced.pending.push_back(name.clone());
			return Ok(track::Consumer::spliced(name, consumer));
		}

		// Reuse a live producer if one is already publishing the track. `get` drops a
		// closed entry and returns `None`, so we fall through to a fresh request.
		if let Some(weak) = state.tracks.get(name) {
			return Ok(weak.consume());
		}

		if let Some(pending) = state.requests.join(name) {
			// Coalesce onto a queued request for the same name.
			return Ok(pending.consume());
		}

		// A deliberately-ended broadcast serves nothing new; existing tracks above
		// stay readable so consumers can drain the cache.
		if state.closing {
			return Err(Error::NotFound);
		}

		// Allocate the name once and share the same Arc across the request, the
		// requests map, and the FIFO order. The request inherits the broadcast's
		// cache pool through its `Arc<Info>`, same as a producer-created track.
		let name: Arc<str> = name.into();
		let request = track::Request::new(self.info.clone(), name.clone());
		let consumer = request.consume();

		// With no handler alive to serve it, the request is dropped: `NotFound` beats
		// handing back a consumer that would only resolve `Dropped`.
		if state.requests.insert(name, request).is_err() {
			return Err(Error::NotFound);
		}

		Ok(consumer)
	}

	/// A watch-only handle to the broadcast's demand. See [`Demand`].
	pub(crate) fn demand(&self) -> Demand {
		Demand {
			alive: self.alive.weak(),
			state: self.state.clone(),
		}
	}

	/// Block until the broadcast is closed (every producer dropped) and return the cause.
	///
	/// Always returns [`Error::Dropped`]: a broadcast is just a collection of tracks, so it
	/// only ends when every producer is gone. There is no way to abort it with a code.
	pub async fn closed(&self) -> Error {
		self.alive.closed().await;
		Error::Dropped
	}

	/// Returns true if every [`Producer`] has been dropped.
	pub fn is_closed(&self) -> bool {
		self.alive.is_closed()
	}

	/// Whether the broadcast is on its way out: deliberately ended (finish/abort
	/// marked, even while handles remain) or already fully closed. The origin's
	/// dispatcher treats a rejection from such a source as imminent detach rather
	/// than a strike.
	pub(crate) fn is_closing(&self) -> bool {
		self.is_closed() || self.state.read().closing
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
	pub(crate) fn weak(&self) -> WeakConsumer {
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
pub(crate) struct WeakConsumer {
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
			route_seen: None,
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
/// Obtained from [`Producer::demand`]; the broadcast-level sibling of
/// [`track::Demand`](crate::track::Demand). Demand means live interest in the
/// broadcast's content: a subscribed spliced track on a route-fed broadcast, or
/// a pending track request / a consumed track on an ordinary one. A publisher
/// uses it to run expensive work only while someone is watching, and routing
/// uses it to advertise a warm copy at zero cost.
///
/// It's a weak handle: it neither keeps the broadcast alive nor counts as
/// demand itself. Once every producer is gone, [`used`](Self::used) /
/// [`unused`](Self::unused) return [`Error::Dropped`].
#[derive(Clone)]
pub struct Demand {
	alive: kio::ConsumerWeak<()>,
	state: kio::Shared<BroadcastState>,
}

impl Demand {
	/// Whether the broadcast has live demand right now.
	///
	/// A point-in-time snapshot with no registration; use [`Self::used`] /
	/// [`Self::unused`] (or their `poll_*` forms) to wait for the edge.
	pub fn is_used(&self) -> bool {
		self.state.read().is_used()
	}

	/// Block until the broadcast has demand. Resolves immediately if it already
	/// does; returns [`Error::Dropped`] once every producer is gone.
	pub async fn used(&self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_used(waiter)).await
	}

	/// Block until the broadcast has no demand. Resolves immediately if it has
	/// none; returns [`Error::Dropped`] once every producer is gone.
	pub async fn unused(&self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll_unused(waiter)).await
	}

	/// Poll-based variant of [`Self::used`].
	pub fn poll_used(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_demand(waiter, true)
	}

	/// Poll-based variant of [`Self::unused`].
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_demand(waiter, false)
	}

	fn poll_demand(&self, waiter: &kio::Waiter, want: bool) -> Poll<Result<(), Error>> {
		// Closure is checked first, matching `track::Demand`: a dead broadcast
		// reports Dropped rather than pretending to answer.
		if self.alive.poll_closed(waiter).is_ready() {
			return Poll::Ready(Err(Error::Dropped));
		}
		let ready = self.state.poll(waiter, |state| {
			// The consumer counts live on the per-track channels, whose flips
			// don't write this state: park on those channels too so the edge
			// wakes us, then recompute here.
			state.register_demand(waiter, want);
			match state.is_used() == want {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		});
		match ready {
			Poll::Ready(_) => Poll::Ready(Ok(())),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl Consumer {
	pub fn assert_not_closed(&self) {
		assert!(self.closed().now_or_never().is_none(), "should not be closed");
	}

	pub fn assert_closed(&self) {
		assert!(self.closed().now_or_never().is_some(), "should be closed");
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Await with a timeout so a missed demand wake fails the test instead of
	/// hanging it (time is paused, so the timeout fires instantly when idle).
	async fn expect<T>(fut: impl Future<Output = T>) -> T {
		tokio::time::timeout(std::time::Duration::from_secs(1), fut)
			.await
			.expect("timed out waiting for a demand edge")
	}

	/// Demand on an ordinary broadcast tracks subscriber interest, not
	/// production: a live track producer alone is unused, a consumed track is
	/// used, and both edges wake parked waiters.
	#[tokio::test]
	async fn demand_ordinary() {
		tokio::time::pause();

		let mut producer = Info::new().produce();
		let consumer = producer.consume();
		let demand = producer.demand();

		// No demand yet; `unused` resolves immediately.
		assert!(!demand.is_used());
		demand.unused().await.unwrap();

		// Producing alone is not demand.
		let _track = producer.create_track("a", None).unwrap();
		assert!(!demand.is_used());

		// A consumer appearing wakes a parked `used`.
		let (used, handle) = tokio::join!(expect(demand.used()), async { consumer.track("a").unwrap() });
		used.unwrap();
		assert!(demand.is_used());

		// The last consumer dropping wakes a parked `unused`.
		let (unused, ()) = tokio::join!(expect(demand.unused()), async { drop(handle) });
		unused.unwrap();
		assert!(!demand.is_used());

		// Every producer gone: both edges report the closure.
		producer.finish();
		assert!(matches!(demand.used().await, Err(Error::Dropped)));
		assert!(matches!(demand.unused().await, Err(Error::Dropped)));
	}

	/// Demand on a spliced (route-fed) broadcast follows the logical tracks'
	/// consumers, which is what flips a relay's advertised cost.
	#[tokio::test]
	async fn demand_spliced() {
		tokio::time::pause();

		let producer = Producer::new_spliced(Info::new());
		let consumer = producer.consume();
		let demand = producer.demand();

		assert!(!demand.is_used());
		let track = consumer.track("video").unwrap();
		assert!(demand.is_used());

		// Dropping the only consumer wakes a parked `unused`, even though the
		// logical track itself stays cached in the broadcast.
		let (unused, ()) = tokio::join!(expect(demand.unused()), async { drop(track) });
		unused.unwrap();
		assert!(!demand.is_used());

		// A repeat consumer for the cached track counts again.
		let _track = consumer.track("video").unwrap();
		assert!(demand.is_used());
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

	#[tokio::test]
	async fn insert() {
		let mut producer = Info::new().produce();

		// Create the track before any consumer exists.
		let mut track1 = producer.assert_create_track("track1", None);
		track1.append_group().unwrap();

		let consumer = producer.consume();

		// The track already exists, so subscribe resolves immediately.
		let mut track1_sub = consumer.track("track1").unwrap().subscribe(None).await.unwrap();
		track1_sub.assert_group();

		let mut track2 = producer.assert_create_track("track2", None);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.track("track2").unwrap().subscribe(None).await.unwrap();
		track2_consumer.assert_no_group();

		track2.append_group().unwrap();

		track2_consumer.assert_group();
	}

	#[tokio::test]
	async fn closed() {
		let mut producer = Info::new().produce();
		let dynamic = producer.dynamic();

		let consumer = producer.consume();
		consumer.assert_not_closed();

		// Create a new track and insert it into the broadcast (resolves immediately).
		let track1 = producer.assert_create_track("track1", None);
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

	#[tokio::test]
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
		let mut track3 = request.accept(None);
		let mut track1 = track1_fut.await.unwrap();
		let mut track2 = track2_fut.await.unwrap();

		track1.assert_not_closed();
		track1.assert_is_clone(&track2);
		track3.subscribe(None).assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().unwrap();
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

	#[tokio::test]
	async fn stale_producer() {
		let mut broadcast = Info::new().produce().dynamic();
		let consumer = broadcast.consume();

		// Subscribe to a track and serve it.
		let track1_fut = subscribe_pending!(consumer, "track1");
		let mut producer1 = broadcast.assert_request().accept(None);
		let mut track1 = track1_fut.await.unwrap();

		// Close the producer (simulating publisher disconnect).
		producer1.append_group().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// The consumer should see the track as closed.
		track1.assert_closed();

		// Subscribe again to the same track: should get a NEW producer, not the stale one.
		let track2_fut = subscribe_pending!(consumer, "track1");
		let mut producer2 = broadcast.assert_request().accept(None);
		let mut track2 = track2_fut.await.unwrap();
		track2.assert_not_closed();
		track2.assert_not_clone(&track1);

		// The new consumer should receive the new group.
		producer2.append_group().unwrap();
		track2.assert_group();
	}

	#[tokio::test(start_paused = true)]
	async fn requested_unused() {
		let mut broadcast = Info::new().produce().dynamic();
		let bc = broadcast.consume();

		// Subscribe to a track that doesn't exist yet, then serve it.
		let c1_fut = subscribe_pending!(bc, "unknown_track");
		let mut producer1 = broadcast.assert_request().accept(None);
		let consumer1 = c1_fut.await.unwrap();

		// The producer should NOT be unused yet because there's a consumer.
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// A second subscriber reuses the live producer (fast path / dedup).
		let consumer2 = bc.track("unknown_track").unwrap().subscribe(None).await.unwrap();
		consumer2.assert_is_clone(&consumer1);

		drop(consumer1);
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		drop(consumer2);
		assert!(
			producer1.unused().now_or_never().is_some(),
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
		let producer2 = broadcast.assert_request().accept(None);
		let consumer4 = c4_fut.await.unwrap();
		drop(consumer4);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"new track producer should be unused after its consumer is dropped"
		);
	}

	// Cloning a `Consumer` resets its route cursor: a clone that inherited the
	// original's `route_seen` would skip the initial-value delivery that
	// `route_changed` promises.
	#[tokio::test]
	async fn route_clone_observes_current_route() {
		let mut producer = Info::new().produce();
		let mut consumer = producer.consume();

		// Drain the initial route, then a change.
		consumer.route_changed().await.unwrap();
		let route = Route::new().with_cost(7);
		producer.set_route(route.clone()).unwrap();
		assert_eq!(consumer.route_changed().await.unwrap(), route);

		// The original is fully drained: no update pending.
		assert!(consumer.route_changed().now_or_never().is_none());

		// A clone starts fresh, yielding the current route immediately.
		let mut clone = consumer.clone();
		let seen = clone
			.route_changed()
			.now_or_never()
			.expect("clone should observe the current route immediately")
			.unwrap();
		assert_eq!(seen, route);
	}

	// Cloning a `Dynamic` and dropping the clone must not flip the handler
	// count to zero. The relay's lite subscriber clones the
	// dynamic per spawned subscribe; if Clone skipped the increment, the
	// first finished subscribe would tear down the broadcast and any
	// follow-up `track` would return `NotFound`.
	#[tokio::test]
	async fn dynamic_clone_keeps_alive() {
		let broadcast = Info::new().produce().dynamic();
		let consumer = broadcast.consume();

		let clone = broadcast.clone();
		drop(clone);

		// Original handle is still live, so the request registers (stays pending)
		// instead of failing with NotFound.
		let _fut = subscribe_pending!(consumer, "track1");
	}
}
