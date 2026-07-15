use crate::track;
use std::{
	collections::{HashMap, VecDeque},
	sync::Arc,
	task::{Poll, ready},
};

use crate::Error;

use super::{OriginList, WeakCache};

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
	/// available, and end it with [`Producer::close`]. See the note on [`Producer`].
	pub fn produce(self) -> Producer {
		Producer::new(self)
	}
}

/// The path a broadcast takes to reach this origin, and how preferable it is.
///
/// Unlike [`Info`], the route is dynamic: it changes when the serving session fails
/// over, the upstream topology shifts, or the publisher re-advertises itself.
/// Update it with [`Producer::update_route`] and observe changes with
/// [`Consumer::route_updated`]; downstream sessions forward updates as a restart
/// on the wire, so route churn never looks like a new broadcast.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
	/// The chain of origins the broadcast has traversed, oldest first. Each relay
	/// appends its own [`crate::Origin`] when forwarding; used for loop detection
	/// and as the selection tie-break.
	pub hops: OriginList,

	/// Preference among routes serving the same broadcast: lower wins, with ties
	/// broken by hop length and then a deterministic hash. Lets a publisher
	/// advertise how expensive it is to serve (e.g. a standby transcoder), and
	/// change its mind as capacity shifts. Local for now: the wire only carries
	/// hops, so a received route always has the default cost.
	pub cost: u64,
}

impl Route {
	/// A route with the given hop chain and default (best) cost.
	pub fn new(hops: OriginList) -> Self {
		Self { hops, cost: 0 }
	}

	/// Set the route's cost, builder style.
	pub fn with_cost(mut self, cost: u64) -> Self {
		self.cost = cost;
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

	// Pending requests keyed by track name, waiting for the dynamic handler to
	// accept or deny them.
	requests: HashMap<Arc<str>, track::Request>,

	// Requested names in FIFO order for the dynamic handler to drain. A name
	// stays in `requests` (but not here) once handed out as a `track::Request`.
	request_order: VecDeque<Arc<str>>,

	// The current number of dynamic producers.
	// If this is 0, requests must be empty.
	dynamic: usize,

	// Route-fed mode (a relay/origin "front"): tracks are spliced logical tracks
	// joined across per-session tracks. `None` for an ordinary broadcast.
	spliced: Option<SplicedState>,

	// The path the broadcast currently takes to reach us, bumping `route_epoch`
	// on every change so consumers can watch for updates.
	route: Route,
	route_epoch: u64,

	// Set by an explicit `Producer::close()` so `Drop` can tell a deliberate
	// shutdown apart from a producer that was dropped by accident.
	closed: bool,
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
	fn modify(state: &kio::Producer<Self>) -> Result<kio::Mut<'_, Self>, Error> {
		state.write().map_err(|_| Error::Dropped)
	}

	/// Insert a track weak handle into the lookup, returning an error if a live
	/// track already holds the name. A closed entry under the name is reclaimed.
	fn insert_track(&mut self, weak: track::TrackWeak) -> Result<(), Error> {
		match self.tracks.insert(weak.name().clone(), weak) {
			Some(_) => Err(Error::Duplicate),
			None => Ok(()),
		}
	}

	/// Reject any pending dynamic track requests. Called when the last dynamic handler
	/// goes away, so consumers don't block forever on requests nobody will fulfill.
	fn reject_requests(&mut self, err: Error) {
		for (_, request) in self.requests.drain() {
			request.reject(err.clone());
		}
		self.request_order.clear();
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
/// children do *not* keep it alive (cloning a [`Consumer`], holding a
/// [`track::Producer`], or holding the [`crate::origin::Publish`] guard does
/// nothing for the broadcast's lifetime). When the last producer goes away every
/// consumer observes [`Error::Dropped`].
///
/// End the broadcast with [`Self::close`] rather than dropping it. Dropping is an
/// easy footgun in garbage-collected bindings (Go, Python, ...), where the handle
/// can be collected the moment it falls out of scope even while you are still
/// publishing, tearing the stream down mid-broadcast. Dropping the last producer
/// without [`Self::close`] logs a warning.
#[derive(Clone)]
pub struct Producer {
	// Held behind an Arc so each track born from this broadcast can inherit a shared
	// handle (threaded down by [`Self::create_track`] / [`Self::reserve_track`]).
	info: Arc<Info>,
	state: kio::Producer<BroadcastState>,
}

impl Producer {
	/// Create a producer for the given broadcast metadata. Prefer [`Info::produce`].
	pub fn new(info: Info) -> Self {
		Self {
			info: Arc::new(info),
			state: Default::default(),
		}
	}

	/// Create a route-fed (spliced) broadcast: consumer track lookups mint logical
	/// tracks that are spliced across per-session tracks, queued for a route to
	/// serve. Used by the origin for broadcasts reached over the network.
	pub(crate) fn new_spliced(info: Info) -> Self {
		Self {
			info: Arc::new(info),
			state: kio::Producer::new(BroadcastState {
				spliced: Some(SplicedState::default()),
				..Default::default()
			}),
		}
	}

	pub fn info(&self) -> &Info {
		&self.info
	}

	/// The shared metadata handle, threaded into session-local tracks so their
	/// groups reach this broadcast's cache pool.
	pub(crate) fn info_arc(&self) -> Arc<Info> {
		self.info.clone()
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> Result<(), Error> {
		let mut state = BroadcastState::modify(&self.state)?;
		state.tracks.remove(name).ok_or(Error::NotFound)?;
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
		let mut state = BroadcastState::modify(&self.state)?;
		state.insert_track(track.weak())?;
		drop(state);
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
		let mut state = BroadcastState::modify(&self.state)?;
		state.insert_track(request.weak())?;
		drop(state);
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
	/// `with_cache`) before handing the Track to [`Self::create_track`].
	pub fn unique_name(&self, suffix: &str) -> String {
		let state = self.state.read();
		(0u16..)
			.map(|i| format!("{i}{suffix}"))
			.find(|name| !state.tracks.contains_key(name.as_str()))
			.expect("u16 namespace exhausted; wow")
	}

	/// Create a dynamic producer that handles on-demand track requests from consumers.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(self.info.clone(), self.state.clone())
	}

	/// Update the broadcast's [`Route`]: the hop chain and cost it advertises.
	///
	/// Call this when the path to the content changes (an upstream failover) or the
	/// publisher's preference changes (e.g. a transcoder warming up lowers its
	/// cost). Consumers observe the change via [`Consumer::route_updated`] and
	/// sessions forward it downstream as a restart, never as a new broadcast. An
	/// update equal to the current route is a no-op.
	pub fn update_route(&mut self, route: Route) -> Result<(), Error> {
		let mut state = BroadcastState::modify(&self.state)?;
		if state.route == route {
			return Ok(());
		}
		state.route = route;
		state.route_epoch += 1;
		Ok(())
	}

	/// The route the broadcast currently advertises.
	pub fn route(&self) -> Route {
		self.state.read().route.clone()
	}

	/// Poll for the next spliced track awaiting a serving route, returning its name
	/// and logical producer. Route-fed broadcasts only.
	pub(crate) fn poll_spliced_assigned(
		&self,
		waiter: &kio::Waiter,
	) -> Poll<Result<(Arc<str>, super::resume::Producer), Error>> {
		let mut state = match ready!(self.state.poll(waiter, |state| {
			match &state.spliced {
				Some(spliced) if !spliced.pending.is_empty() => Poll::Ready(()),
				_ => Poll::Pending,
			}
		})) {
			Ok(state) => state,
			Err(_) => return Poll::Ready(Err(Error::Dropped)),
		};

		let spliced = state.spliced.as_mut().expect("predicate guaranteed spliced");
		let name = spliced.pending.pop_front().expect("predicate guaranteed a request");
		let producer = spliced.tracks.get(&name).expect("pending name without a track").clone();
		Poll::Ready(Ok((name, producer)))
	}

	/// Re-queue a spliced track for a (new) serving route, e.g. after its previous
	/// route died. Coalesces with an already-queued entry; a no-op if the track no
	/// longer exists.
	pub(crate) fn requeue_spliced(&self, name: &Arc<str>) {
		if let Ok(mut state) = self.state.write()
			&& let Some(spliced) = state.spliced.as_mut()
			&& spliced.tracks.contains_key(name)
			&& !spliced.pending.contains(name)
		{
			spliced.pending.push_back(name.clone());
		}
	}

	/// Abort every spliced track, releasing their subscribers with `err`. Called
	/// when the broadcast closes for good.
	pub(crate) fn abort_spliced(&self, err: Error) {
		if let Ok(mut state) = self.state.write()
			&& let Some(spliced) = state.spliced.as_mut()
		{
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
			state: self.state.consume(),
			route_seen: None,
		}
	}

	/// Cleanly close the broadcast once you are done publishing.
	///
	/// Marks the broadcast as deliberately finished so consumers observe a normal
	/// end. Prefer this over dropping the producer: an accidental drop (see the note
	/// on [`Producer`]) logs a warning, whereas a `close()` is silent.
	///
	/// Only marks intent; the broadcast actually ends once every producer clone is
	/// gone, so a clone that outlives this call keeps it alive until it too is
	/// dropped or closed.
	pub fn close(self) {
		if let Ok(mut state) = self.state.write() {
			state.closed = true;
		}
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		// Only the last producer ending the broadcast matters; a clone dropping
		// leaves it live. Warn if that last exit wasn't an explicit close(), since
		// consumers will then see Error::Dropped (classically a GC-collected handle
		// in a language binding that tears the stream down mid-publish).
		if !self.state.is_last() {
			return;
		}
		if let Ok(state) = self.state.write()
			&& !state.closed
		{
			tracing::warn!(
				"broadcast::Producer dropped without close(). Keep the producer alive while publishing, then call close()."
			);
		}
	}
}

#[cfg(test)]
impl Producer {
	pub fn assert_create_track(
		&mut self,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<track::Info>>,
	) -> track::Producer {
		self.create_track(name, info).expect("should not have errored")
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
	state: kio::Producer<BroadcastState>,
}

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: bump `state.dynamic` so each live handle is counted.
		// Without this, deriving Clone would let `Drop` decrement past `new`'s
		// single increment and prematurely flip `dynamic` to zero, causing
		// future `track` calls to return `NotFound`.
		if let Ok(mut state) = self.state.write() {
			state.dynamic += 1;
		}

		Self {
			info: self.info.clone(),
			state: self.state.clone(),
		}
	}
}

impl Dynamic {
	fn new(info: Arc<Info>, state: kio::Producer<BroadcastState>) -> Self {
		if let Ok(mut state) = state.write() {
			// If the broadcast is already closed, we can't handle any new requests.
			state.dynamic += 1;
		}

		Self { info, state }
	}

	pub fn info(&self) -> &Info {
		&self.info
	}

	// A helper to automatically apply Dropped if the state is closed. The predicate is
	// read-only and just gates readiness; mutate through the returned `Mut`.
	fn poll<F>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<kio::Mut<'_, BroadcastState>, Error>>
	where
		F: FnMut(&kio::Ref<'_, BroadcastState>) -> Poll<()>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(state) => Ok(state),
			Err(_) => Err(Error::Dropped),
		})
	}

	/// Poll for the next consumer-requested track, without blocking.
	pub fn poll_requested_track(&mut self, waiter: &kio::Waiter) -> Poll<Result<track::Request, Error>> {
		let mut state = ready!(self.poll(waiter, |state| {
			if state.request_order.is_empty() {
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		}))?;

		let name = state.request_order.pop_front().expect("predicate guaranteed a request");
		let pending = state.requests.remove(&name).expect("request_order out of sync");
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
			state: self.state.consume(),
			route_seen: None,
		}
	}

	/// Block until the broadcast is closed (every producer dropped), returning the cause.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		Error::Dropped
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for Dynamic {
	fn drop(&mut self) {
		if let Ok(mut state) = self.state.write() {
			// We do a saturating sub so Producer::dynamic() can avoid returning an error.
			state.dynamic = state.dynamic.saturating_sub(1);
			if state.dynamic != 0 {
				return;
			}

			// No dynamic handlers left to fulfill pending requests; reject them.
			state.reject_requests(Error::Dropped);
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
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
#[derive(Clone)]
pub struct Consumer {
	info: Arc<Info>,
	state: kio::Consumer<BroadcastState>,
	// The route epoch last yielded by `route_updated`, so each consumer clone
	// observes the current route first and every change after it exactly once.
	route_seen: Option<u64>,
}

impl Consumer {
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// The [`Route`] the broadcast currently takes to reach this origin.
	pub fn route(&self) -> Route {
		self.state.read().route.clone()
	}

	/// Poll for a route change. See [`Self::route_updated`].
	pub fn poll_route_updated(&mut self, waiter: &kio::Waiter) -> Poll<Result<Route, Error>> {
		let seen = self.route_seen;
		let route = match ready!(self.state.poll(waiter, |state| {
			if seen != Some(state.route_epoch) {
				Poll::Ready((state.route.clone(), state.route_epoch))
			} else {
				Poll::Pending
			}
		})) {
			Ok((route, epoch)) => {
				self.route_seen = Some(epoch);
				route
			}
			Err(_) => return Poll::Ready(Err(Error::Dropped)),
		};
		Poll::Ready(Ok(route))
	}

	/// Wait for the broadcast's [`Route`] to change.
	///
	/// The first call returns the current route immediately; each later call blocks
	/// until it changes again, so a loop observes the initial value followed by
	/// every update. Returns [`Error::Dropped`] once every producer is gone.
	pub async fn route_updated(&mut self) -> Result<Route, Error> {
		kio::wait(|waiter| self.poll_route_updated(waiter)).await
	}

	/// Get a handle to a track on this broadcast.
	pub fn track(&self, name: &str) -> Result<track::Consumer, Error> {
		// Upgrade to a temporary producer so we can modify the state.
		let mut state = match self.state.write() {
			Ok(state) => state,
			Err(_) => return Err(Error::Dropped),
		};

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

		if let Some(pending) = state.requests.get_mut(name) {
			// Coalesce onto an in-flight request for the same name.
			return Ok(pending.consume());
		}

		if state.dynamic == 0 {
			return Err(Error::NotFound);
		}

		// Allocate the name once and share the same Arc across the request, the
		// requests map, and the FIFO order. The request inherits the broadcast's
		// cache pool through its `Arc<Info>`, same as a producer-created track.
		let name: Arc<str> = name.into();
		let request = track::Request::new(self.info.clone(), name.clone());
		let consumer = request.consume();

		state.requests.insert(name.clone(), request);
		state.request_order.push_back(name);

		Ok(consumer)
	}

	/// Block until the broadcast is closed (every producer dropped) and return the cause.
	///
	/// Always returns [`Error::Dropped`]: a broadcast is just a collection of tracks, so it
	/// only ends when every producer is gone. There is no way to abort it with a code.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		Error::Dropped
	}

	/// Returns true if every [`Producer`] has been dropped.
	pub fn is_closed(&self) -> bool {
		self.state.read().is_closed()
	}

	/// Register a [`kio::Waiter`] that fires when the broadcast closes.
	///
	/// Returns [`Poll::Ready`] if already closed, otherwise [`Poll::Pending`] after
	/// arming the waiter. Useful for composing close-detection into a larger poll
	/// without spawning a task per broadcast.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_closed(waiter)
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
			state: self.state.weak(),
		}
	}
}

/// A weak reference to a broadcast that doesn't prevent it from closing.
///
/// Mirrors [`track::TrackWeak`]: held by the origin's dynamic cache to share one
/// dynamically-served broadcast across repeat requests without pinning it alive.
#[derive(Clone)]
pub(crate) struct WeakConsumer {
	info: Arc<Info>,
	state: kio::Weak<BroadcastState>,
}

impl WeakConsumer {
	/// Upgrade to a full [`Consumer`] sharing the same broadcast state.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info.clone(),
			state: self.state.consume(),
			route_seen: None,
		}
	}
}

impl super::WeakEntry for WeakConsumer {
	fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

#[cfg(test)]
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

	// Cloning a `Dynamic` and dropping the clone must not flip
	// `state.dynamic` to zero. The relay's lite subscriber clones the
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
