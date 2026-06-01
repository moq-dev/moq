use std::{
	collections::{HashMap, VecDeque, hash_map},
	ops::Deref,
	task::{Poll, ready},
};

use crate::{Error, Subscription, TrackProducer, TrackSubscriber, model::track::TrackWeak};

use super::{OriginList, Track};

/// A collection of media tracks that can be published and subscribed to.
///
/// Create via [`Broadcast::produce`] to obtain both [`BroadcastProducer`] and [`BroadcastConsumer`] pair.
#[derive(Clone, Debug, Default)]
pub struct Broadcast {
	/// The chain of origins the broadcast has traversed. Each relay appends its own
	/// [`crate::Origin`] when forwarding, so the list is used for loop detection and
	/// shortest-path preference.
	pub hops: OriginList,
}

impl Broadcast {
	/// Create a new broadcast with an empty hop chain.
	pub fn new() -> Self {
		Self::default()
	}

	/// Consume this [Broadcast] to create a producer that carries its metadata
	/// (including the hop chain).
	pub fn produce(self) -> BroadcastProducer {
		BroadcastProducer::new(self)
	}
}

/// The slot a pending subscription resolves into: `None` until the publisher
/// accepts (delivering the consumer) or denies (delivering an error). Carried by
/// a [`kio`] channel so subscribers can `poll_ok` it without tokio. The consumer
/// is created at accept time so it counts toward the producer immediately, before
/// the subscriber even polls.
type PendingSlot = Option<Result<TrackSubscriber, Error>>;

/// One waiting subscriber: its preferences and the producer side of its resolver channel.
type Resolver = (Subscription, kio::Producer<PendingSlot>);

/// A track that has been subscribed to but not yet served by the dynamic handler.
///
/// Multiple subscribers to the same name before it is accepted coalesce into one
/// pending request, each adding a resolver channel so they all receive a consumer
/// for the same producer once it is accepted.
#[derive(Default)]
struct PendingRequest {
	resolvers: Vec<Resolver>,
}

/// Resolve every waiting subscriber with `err`.
fn fail_resolvers(resolvers: Vec<Resolver>, err: &Error) {
	for (_, slot) in resolvers {
		if let Ok(mut slot) = slot.write() {
			*slot = Some(Err(err.clone()));
		}
	}
}

#[derive(Default)]
struct State {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	tracks: HashMap<String, TrackWeak>,

	// Pending requests keyed by track name, waiting for the dynamic handler to
	// accept or deny them.
	requests: HashMap<String, PendingRequest>,

	// Requested names in FIFO order for the dynamic handler to drain. A name
	// stays in `requests` (but not here) once handed out as a `TrackRequest`.
	request_order: VecDeque<String>,

	// The current number of dynamic producers.
	// If this is 0, requests must be empty.
	dynamic: usize,

	// The error that caused the broadcast to be aborted, if any.
	abort: Option<Error>,
}

fn modify(state: &kio::Producer<State>) -> Result<kio::Mut<'_, State>, Error> {
	match state.write() {
		Ok(state) => Ok(state),
		Err(r) => Err(r.abort.clone().unwrap_or(Error::Dropped)),
	}
}

impl State {
	/// Insert a track weak handle into the lookup, returning an error on duplicate.
	fn insert_track(&mut self, weak: TrackWeak) -> Result<(), Error> {
		let hash_map::Entry::Vacant(entry) = self.tracks.entry(weak.info.name.clone()) else {
			return Err(Error::Duplicate);
		};
		entry.insert(weak);
		Ok(())
	}

	/// Drop every pending request, notifying all waiting subscribers with `err`.
	fn abort_requests(&mut self, err: &Error) {
		self.request_order.clear();
		for (_, pending) in self.requests.drain() {
			fail_resolvers(pending.resolvers, err);
		}
	}

	/// Drop a single named pending request, notifying its subscribers with `err`.
	fn deny_request(&mut self, name: &str, err: Error) {
		self.request_order.retain(|n| n != name);
		if let Some(pending) = self.requests.remove(name) {
			fail_resolvers(pending.resolvers, &err);
		}
	}
}

/// Manages tracks within a broadcast.
///
/// Insert tracks statically with [Self::insert_track] / [Self::create_track],
/// or handle on-demand requests via [Self::dynamic].
#[derive(Clone)]
pub struct BroadcastProducer {
	info: Broadcast,
	state: kio::Producer<State>,
}

impl Deref for BroadcastProducer {
	type Target = Broadcast;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl BroadcastProducer {
	/// Create a producer for the given broadcast metadata. Prefer [`Broadcast::produce`].
	pub fn new(info: Broadcast) -> Self {
		Self {
			info,
			state: Default::default(),
		}
	}

	/// Insert a track into the lookup, returning an error on duplicate.
	///
	/// Stores a weak handle to the track. The caller (or the owner of the
	/// track's [`TrackProducer`]) is responsible for keeping the track alive;
	/// when all producers are dropped, the entry becomes closed and is
	/// eventually evicted.
	pub fn insert_track(&mut self, track: TrackSubscriber) -> Result<(), Error> {
		let mut state = modify(&self.state)?;
		state.insert_track(track.weak())
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> Result<(), Error> {
		let mut state = modify(&self.state)?;
		state.tracks.remove(name).ok_or(Error::NotFound)?;
		Ok(())
	}

	/// Produce a new track and insert it into the broadcast.
	///
	/// Accepts anything that converts into a [`Track`], so a bare name works:
	/// `create_track("video")`.
	pub fn create_track(&mut self, track: impl Into<Track>) -> Result<TrackProducer, Error> {
		let track = TrackProducer::new(track.into());
		let mut state = modify(&self.state)?;
		state.insert_track(track.weak())?;
		drop(state);
		Ok(track)
	}

	/// Create a track with a unique name using the given suffix.
	///
	/// Generates names like `0{suffix}`, `1{suffix}`, etc. and picks the first
	/// one not already used in this broadcast.
	pub fn unique_track(&mut self, suffix: &str) -> Result<TrackProducer, Error> {
		let name = self.unique_name(suffix);
		self.create_track(Track::new(name))
	}

	/// Generate a unique track name from a suffix without creating the track.
	///
	/// Returns a fresh name like `0{suffix}`, `1{suffix}`, etc. Use this when
	/// you need to set non-default Track properties (e.g. `with_timescale`,
	/// `with_compress`) before handing the Track to [`Self::create_track`].
	pub fn unique_name(&self, suffix: &str) -> String {
		let state = self.state.read();
		(0u32..)
			.map(|i| format!("{i}{suffix}"))
			.find(|name| !state.tracks.contains_key(name))
			.expect("u32 namespace exhausted")
	}

	/// Create a dynamic producer that handles on-demand track requests from consumers.
	pub fn dynamic(&self) -> BroadcastDynamic {
		BroadcastDynamic::new(self.info.clone(), self.state.clone())
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
		}
	}

	/// Abort the broadcast with the given error.
	///
	/// Externally-owned tracks are independent and must be aborted separately;
	/// inserted tracks are referenced via weak handles so that consumers can
	/// finish reading them. Pending dynamic track requests, however, are owned
	/// by the broadcast and have no other producer to fulfill them, so they are
	/// aborted here.
	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut guard = modify(&self.state)?;

		// Wake any pending subscribers; nothing will ever serve their requests.
		guard.abort_requests(&err);

		guard.abort = Some(err);
		guard.close();
		Ok(())
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

#[cfg(test)]
impl BroadcastProducer {
	pub fn assert_create_track(&mut self, track: &Track) -> TrackProducer {
		self.create_track(track.clone()).expect("should not have errored")
	}

	pub fn assert_insert_track(&mut self, track: &TrackProducer) {
		self.insert_track(track.subscribe_default())
			.expect("should not have errored")
	}
}

/// A subscription waiting to be served, handed out by [`BroadcastDynamic::requested_track`].
///
/// The publisher inspects [`Self::name`] (and optionally [`Self::subscription`]),
/// then either [`Self::accept`]s it with a concrete [`Track`], which resolves all
/// waiting subscribers, or [`Self::deny`]s it. Dropping without doing either
/// denies with [`Error::Cancel`].
pub struct TrackRequest {
	name: String,
	subscription: Subscription,
	state: kio::Weak<State>,
	/// Set once accepted or denied so [`Drop`] doesn't deny a second time.
	completed: bool,
}

impl TrackRequest {
	/// The requested track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The first waiting subscriber's preferences, as a hint for constructing the
	/// [`Track`]. The full aggregate is available on the [`TrackProducer`] returned
	/// by [`Self::accept`] via [`TrackProducer::subscription`].
	pub fn subscription(&self) -> &Subscription {
		&self.subscription
	}

	/// Serve the request with the given track, resolving every waiting subscriber.
	///
	/// The track's name must match [`Self::name`]. Returns [`Error::NotFound`] on
	/// mismatch, or the broadcast's abort error if it closed while pending.
	pub fn accept(mut self, track: Track) -> Result<TrackProducer, Error> {
		if track.name != self.name {
			return Err(Error::NotFound);
		}
		self.completed = true;

		let mut state = self
			.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Cancel))?;

		let pending = state.requests.remove(&self.name).ok_or(Error::Cancel)?;
		state.request_order.retain(|n| n != &self.name);

		let producer = TrackProducer::new(track);

		// Insert a weak reference so future subscribers dedupe onto this producer.
		state.tracks.insert(self.name.clone(), producer.weak());

		// Hand each waiting subscriber a consumer carrying its own preferences.
		// Building it here (not when the subscriber polls) means it counts toward
		// the producer immediately, so a publisher checking `unused()` right after
		// accept doesn't see zero consumers and tear the track down.
		for (subscription, slot) in pending.resolvers {
			if let Ok(mut slot) = slot.write() {
				*slot = Some(Ok(producer.subscribe(subscription)));
			}
		}

		drop(state);

		// Evict the lookup entry once the producer closes. The producer owner is
		// responsible for closing it (abort/finish/drop); waiting on close rather
		// than unused lets a producer linger across transient consumer churn (relay
		// subscription churn) without losing the lookup entry to an auto-cleanup race.
		let weak = producer.weak();
		let cleanup = self.state.clone();
		web_async::spawn(async move {
			weak.closed().await;

			let Some(producer) = cleanup.produce() else {
				return;
			};
			let Ok(mut state) = producer.write() else {
				return;
			};

			// Remove the entry, but reinsert if it was replaced by a newer producer.
			if let Some(current) = state.tracks.remove(&weak.info.name)
				&& !current.is_clone(&weak)
			{
				state.tracks.insert(current.info.name.clone(), current);
			}
		});

		Ok(producer)
	}

	/// Reject the request, waking all waiting subscribers with `err`.
	pub fn deny(mut self, err: Error) {
		self.completed = true;
		if let Ok(mut state) = self.state.write() {
			state.deny_request(&self.name, err);
		}
	}
}

impl Drop for TrackRequest {
	fn drop(&mut self) {
		if !self.completed
			&& let Ok(mut state) = self.state.write()
		{
			state.deny_request(&self.name, Error::Cancel);
		}
	}
}

/// A pending subscription returned by [`TrackConsumer::subscribe`].
///
/// The subscription isn't live until the publisher accepts it (for the wire,
/// SUBSCRIBE_OK). It implements [`Future`], so `.await` it to get the
/// [`TrackSubscriber`] (or an error). Poll-based callers can instead drive it
/// with [`Self::poll_ok`] inside a `kio` poll loop.
pub struct TrackPending {
	inner: TrackPendingInner,
	/// Kept alive between `Future::poll` calls so its registration in the
	/// resolver channel stays valid until the next poll replaces it.
	waiter: Option<kio::Waiter>,
}

enum TrackPendingInner {
	/// Resolved synchronously: the track already existed, or it failed immediately.
	Ready(Result<TrackSubscriber, Error>),
	/// Waiting for the publisher to accept or deny via the dynamic handler.
	Waiting(kio::Consumer<PendingSlot>),
}

impl TrackPending {
	fn ready(result: Result<TrackSubscriber, Error>) -> Self {
		Self {
			inner: TrackPendingInner::Ready(result),
			waiter: None,
		}
	}

	fn waiting(consumer: kio::Consumer<PendingSlot>) -> Self {
		Self {
			inner: TrackPendingInner::Waiting(consumer),
			waiter: None,
		}
	}

	/// Poll for the resolved [`TrackSubscriber`], without blocking.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<TrackSubscriber, Error>> {
		match &self.inner {
			TrackPendingInner::Ready(result) => Poll::Ready(result.clone()),
			TrackPendingInner::Waiting(consumer) => match consumer.poll(waiter, |slot| match &**slot {
				Some(result) => Poll::Ready(result.clone()),
				None => Poll::Pending,
			}) {
				Poll::Ready(Ok(result)) => Poll::Ready(result),
				// Channel closed: the resolver may have left the final result behind.
				Poll::Ready(Err(closed)) => Poll::Ready(match &*closed {
					Some(result) => result.clone(),
					None => Err(Error::Cancel),
				}),
				Poll::Pending => Poll::Pending,
			},
		}
	}
}

impl std::future::Future for TrackPending {
	type Output = Result<TrackSubscriber, Error>;

	fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		// Replacing drops the previous waiter, freeing its slot so poll_ok's
		// register call can recycle it (see `kio::wait`).
		this.waiter = Some(kio::Waiter::new(cx.waker().clone()));
		this.poll_ok(this.waiter.as_ref().unwrap())
	}
}

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, the dynamic producer
/// picks up the request via [`Self::requested_track`] and either
/// [`TrackRequest::accept`]s it with a concrete [`Track`] or
/// [`TrackRequest::deny`]s it. Dropped when no longer needed; pending requests
/// are automatically aborted.
pub struct BroadcastDynamic {
	info: Broadcast,
	state: kio::Producer<State>,
}

impl Clone for BroadcastDynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: bump `state.dynamic` so each live handle is counted.
		// Without this, deriving Clone would let `Drop` decrement past `new`'s
		// single increment and prematurely flip `dynamic` to zero, causing
		// future `consume_track` calls to return `NotFound`.
		if let Ok(mut state) = self.state.write() {
			state.dynamic += 1;
		}

		Self {
			info: self.info.clone(),
			state: self.state.clone(),
		}
	}
}

impl Deref for BroadcastDynamic {
	type Target = Broadcast;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl BroadcastDynamic {
	fn new(info: Broadcast, state: kio::Producer<State>) -> Self {
		if let Ok(mut state) = state.write() {
			// If the broadcast is already closed, we can't handle any new requests.
			state.dynamic += 1;
		}

		Self { info, state }
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R, Error>>
	where
		F: FnMut(&mut kio::Mut<'_, State>) -> Poll<R>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(r) => Ok(r),
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for the next consumer-requested track, without blocking.
	pub fn poll_requested_track(&mut self, waiter: &kio::Waiter) -> Poll<Result<TrackRequest, Error>> {
		let weak = self.state.weak();
		self.poll(waiter, |state| {
			let Some(name) = state.request_order.pop_front() else {
				return Poll::Pending;
			};
			// The name stays in `requests` so concurrent subscribers can still
			// coalesce onto it until the publisher accepts or denies.
			let pending = state.requests.get(&name).expect("request_order out of sync");
			let subscription = pending.resolvers.first().map(|(s, _)| s.clone()).unwrap_or_default();
			Poll::Ready((name, subscription))
		})
		.map(|res| {
			res.map(|(name, subscription)| TrackRequest {
				name,
				subscription,
				state: weak,
				completed: false,
			})
		})
	}

	/// Block until a consumer requests a track, returning a [`TrackRequest`] to serve.
	pub async fn requested_track(&mut self) -> Result<TrackRequest, Error> {
		kio::wait(|waiter| self.poll_requested_track(waiter)).await
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
		}
	}

	/// Block until the broadcast is closed or aborted, returning the cause.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}

	/// Abort the broadcast with the given error.
	///
	/// Externally-owned tracks are independent and must be aborted separately;
	/// inserted tracks are referenced via weak handles. Pending dynamic track
	/// requests are owned by the broadcast and aborted here so consumers don't
	/// stay stuck waiting on producers nobody will fulfill.
	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut guard = modify(&self.state)?;

		// Wake any pending subscribers; nothing will ever serve their requests.
		guard.abort_requests(&err);

		guard.abort = Some(err);
		guard.close();
		Ok(())
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for BroadcastDynamic {
	fn drop(&mut self) {
		if let Ok(mut state) = self.state.write() {
			// We do a saturating sub so Producer::dynamic() can avoid returning an error.
			state.dynamic = state.dynamic.saturating_sub(1);
			if state.dynamic != 0 {
				return;
			}

			// No dynamic producer remains to serve pending requests.
			state.abort_requests(&Error::Cancel);
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl BroadcastDynamic {
	pub fn assert_request(&mut self) -> TrackRequest {
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
pub struct BroadcastConsumer {
	info: Broadcast,
	state: kio::Consumer<State>,
}

impl Deref for BroadcastConsumer {
	type Target = Broadcast;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl BroadcastConsumer {
	/// Get a handle to a track on this broadcast.
	///
	/// This is a cheap, synchronous lookup that returns a [`TrackConsumer`] bound
	/// to `name`. Nothing is sent to the publisher yet: call
	/// [`TrackConsumer::subscribe`] to open a live subscription (blocking on
	/// SUBSCRIBE_OK), or hold the handle and subscribe later.
	pub fn consume_track(&self, name: &str) -> TrackConsumer {
		TrackConsumer {
			broadcast: self.clone(),
			name: name.to_string(),
		}
	}

	/// Register a subscription for `name` and return a [`TrackPending`] that
	/// resolves once the publisher accepts it.
	///
	/// Reuses a live producer if one is already publishing the track (the pending
	/// resolves right away), otherwise queues a dynamic request served via
	/// [`BroadcastDynamic::requested_track`] and [`TrackRequest::accept`] (for the
	/// wire this is SUBSCRIBE_OK). Resolves to [`Error::NotFound`] if no dynamic
	/// producer exists to handle the request.
	fn request_subscribe(&self, name: &str, subscription: Subscription) -> TrackPending {
		// Upgrade to a temporary producer so we can modify the state.
		let Some(producer) = self.state.produce() else {
			let err = self.state.read().abort.clone().unwrap_or(Error::Dropped);
			return TrackPending::ready(Err(err));
		};
		let mut state = match modify(&producer) {
			Ok(state) => state,
			Err(err) => return TrackPending::ready(Err(err)),
		};

		// Reuse a live producer if one is already publishing the track.
		if let Some(weak) = state.tracks.get(name) {
			if !weak.is_closed() {
				return TrackPending::ready(Ok(weak.subscribe(subscription)));
			}
			// Drop the stale entry and fall through to a fresh request.
			state.tracks.remove(name);
		}

		let slot = kio::Producer::new(None);
		let consumer = slot.consume();

		if let Some(pending) = state.requests.get_mut(name) {
			// Coalesce onto an in-flight request for the same name.
			pending.resolvers.push((subscription, slot));
		} else if state.dynamic == 0 {
			return TrackPending::ready(Err(Error::NotFound));
		} else {
			state.requests.insert(
				name.to_string(),
				PendingRequest {
					resolvers: vec![(subscription, slot)],
				},
			);
			state.request_order.push_back(name.to_string());
		}

		TrackPending::waiting(consumer)
	}

	/// Block until the broadcast is closed and return the cause.
	///
	/// Returns [`Error::Dropped`] if every producer was dropped without an
	/// explicit abort, or the abort error supplied by [`BroadcastProducer::abort`].
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}

	/// Returns true if every [`BroadcastProducer`] has been dropped.
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
}

/// A handle to a single track within a broadcast.
///
/// Obtained from [`BroadcastConsumer::consume_track`]. Holding it sends nothing
/// to the publisher; it just names a track you can [`subscribe`](Self::subscribe)
/// to (a live, ongoing stream of groups) later. The same handle can be subscribed
/// to multiple times, and clones are cheap.
// TODO: add `fetch` for one-shot retrieval of a past group range.
#[derive(Clone)]
pub struct TrackConsumer {
	broadcast: BroadcastConsumer,
	name: String,
}

impl TrackConsumer {
	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Open a live subscription.
	///
	/// Returns a [`TrackPending`] that resolves once the publisher accepts the
	/// subscription (SUBSCRIBE_OK on the wire). `.await` it for the
	/// [`TrackSubscriber`], which carries the publisher's [`Track`] and reads its
	/// groups; or drive it with [`TrackPending::poll_ok`] from a poll loop.
	///
	/// `subscription` is this subscriber's preferences and feeds the producer's
	/// [`TrackProducer::subscription`] aggregate. Concurrent subscribers to the
	/// same name coalesce onto one request.
	pub fn subscribe(&self, subscription: Subscription) -> TrackPending {
		self.broadcast.request_subscribe(&self.name, subscription)
	}
}

#[cfg(test)]
impl BroadcastConsumer {
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
	/// a publisher accepts). Returns the [`TrackPending`] to resolve after accepting.
	macro_rules! subscribe_pending {
		($consumer:expr, $name:expr) => {{
			let pending = $consumer.consume_track($name).subscribe(Subscription::default());
			assert!(
				pending.poll_ok(&kio::Waiter::noop()).is_pending(),
				"consume_track should stay pending until the request is accepted"
			);
			pending
		}};
	}

	#[tokio::test]
	async fn insert() {
		let mut producer = Broadcast::new().produce();
		let mut track1 = Track::new("track1").produce();

		// Make sure we can insert before a consumer is created.
		producer.assert_insert_track(&track1);
		track1.append_group().unwrap();

		let consumer = producer.consume();

		// The track already exists, so subscribe resolves immediately.
		let mut track1_sub = consumer
			.consume_track("track1")
			.subscribe(Subscription::default())
			.await
			.unwrap();
		track1_sub.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.assert_insert_track(&track2);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2
			.consume_track("track2")
			.subscribe(Subscription::default())
			.await
			.unwrap();
		track2_consumer.assert_no_group();

		track2.append_group().unwrap();

		track2_consumer.assert_group();
	}

	#[tokio::test]
	async fn closed() {
		let mut producer = Broadcast::new().produce();
		let _dynamic = producer.dynamic();

		let consumer = producer.consume();
		consumer.assert_not_closed();

		// Create a new track and insert it into the broadcast (resolves immediately).
		let track1 = producer.assert_create_track(&Track::new("track1"));
		let track1c = consumer
			.consume_track("track1")
			.subscribe(Subscription::default())
			.await
			.unwrap();

		// A track nobody publishes stays pending until accepted.
		let track2_fut = subscribe_pending!(consumer, "track2");

		// Aborting the broadcast must NOT cascade to externally-owned tracks.
		producer.abort(Error::Cancel).unwrap();

		// track2 was a pending dynamic request, so its subscribe surfaces the abort.
		assert!(track2_fut.await.is_err());

		// track1's producer is held outside the broadcast, so it survives.
		assert!(!track1.is_closed());
		track1c.assert_not_closed();
	}

	#[tokio::test]
	async fn requests() {
		let mut producer = Broadcast::new().produce().dynamic();

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
		let mut track3 = request.accept(Track::new("track1")).unwrap();
		let mut track1 = track1_fut.await.unwrap();
		let mut track2 = track2_fut.await.unwrap();

		track1.assert_not_closed();
		track1.assert_is_clone(&track2);
		track3.subscribe_default().assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().unwrap();
		track1.assert_group();
		track2.assert_group();

		// A pending request is cancelled when the dynamic producer is dropped.
		let track4_fut = subscribe_pending!(consumer, "track2");
		drop(producer);
		assert!(track4_fut.await.is_err());

		// With no dynamic producer left, new subscribes fail outright.
		let track5 = consumer2
			.consume_track("track3")
			.subscribe(Subscription::default())
			.await;
		assert!(track5.is_err(), "should have errored");
	}

	#[tokio::test]
	async fn stale_producer() {
		let mut broadcast = Broadcast::new().produce().dynamic();
		let consumer = broadcast.consume();

		// Subscribe to a track and serve it.
		let track1_fut = subscribe_pending!(consumer, "track1");
		let mut producer1 = broadcast.assert_request().accept(Track::new("track1")).unwrap();
		let track1 = track1_fut.await.unwrap();

		// Close the producer (simulating publisher disconnect).
		producer1.append_group().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// The consumer should see the track as closed.
		track1.assert_closed();

		// Subscribe again to the same track: should get a NEW producer, not the stale one.
		let track2_fut = subscribe_pending!(consumer, "track1");
		let mut producer2 = broadcast.assert_request().accept(Track::new("track1")).unwrap();
		let mut track2 = track2_fut.await.unwrap();
		track2.assert_not_closed();
		track2.assert_not_clone(&track1);

		// The new consumer should receive the new group.
		producer2.append_group().unwrap();
		track2.assert_group();
	}

	#[tokio::test(start_paused = true)]
	async fn requested_unused() {
		let mut broadcast = Broadcast::new().produce().dynamic();
		let bc = broadcast.consume();

		// Subscribe to a track that doesn't exist yet, then serve it.
		let c1_fut = subscribe_pending!(bc, "unknown_track");
		let mut producer1 = broadcast.assert_request().accept(Track::new("unknown_track")).unwrap();
		let consumer1 = c1_fut.await.unwrap();

		// The producer should NOT be unused yet because there's a consumer.
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// A second subscriber reuses the live producer (fast path / dedup).
		let consumer2 = bc
			.consume_track("unknown_track")
			.subscribe(Subscription::default())
			.await
			.unwrap();
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
		// it (no new request) — this is what lets the relay linger upstream
		// subscriptions across transient consumer churn.
		let consumer3 = bc
			.consume_track("unknown_track")
			.subscribe(Subscription::default())
			.await
			.unwrap();
		consumer3.assert_is_clone(&producer1.subscribe_default());
		broadcast.assert_no_request();
		drop(consumer3);

		// Aborting the producer triggers cleanup; the next subscribe creates a fresh request.
		producer1.abort(Error::Cancel).unwrap();
		// Yield (paused time) so the spawned cleanup task evicts the lookup entry.
		tokio::time::advance(std::time::Duration::from_millis(1)).await;

		let c4_fut = subscribe_pending!(bc, "unknown_track");
		let producer2 = broadcast.assert_request().accept(Track::new("unknown_track")).unwrap();
		let consumer4 = c4_fut.await.unwrap();
		drop(consumer4);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"new track producer should be unused after its consumer is dropped"
		);
	}

	// Cloning a `BroadcastDynamic` and dropping the clone must not flip
	// `state.dynamic` to zero. The relay's lite subscriber clones the
	// dynamic per spawned subscribe; if Clone skipped the increment, the
	// first finished subscribe would tear down the broadcast and any
	// follow-up `consume_track` would return `NotFound`.
	#[tokio::test]
	async fn dynamic_clone_keeps_alive() {
		let broadcast = Broadcast::new().produce().dynamic();
		let consumer = broadcast.consume();

		let clone = broadcast.clone();
		drop(clone);

		// Original handle is still live, so the request registers (stays pending)
		// instead of failing with NotFound.
		let _fut = subscribe_pending!(consumer, "track1");
	}
}
