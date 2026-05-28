use std::{
	collections::{HashMap, hash_map},
	ops::Deref,
	task::{Poll, ready},
};

use tokio::sync::oneshot;

use crate::{Error, Subscription, TrackConsumer, TrackProducer, model::track::TrackWeak};

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

/// A pending subscription request, waiting for a publisher to call
/// [`TrackRequest::accept`] (or [`TrackRequest::deny`]) to resolve it.
struct PendingRequest {
	subscription: Subscription,
	/// Resolvers waiting for the producer to be created. Each call to
	/// [`BroadcastConsumer::subscribe_track`] for the same name during the
	/// pending window adds an entry here so they all see the same producer.
	resolvers: Vec<oneshot::Sender<Result<TrackConsumer, Error>>>,
}

#[derive(Default)]
struct State {
	/// Weak references to live producers, used to dedupe subscribe_track calls
	/// that target a name already being served.
	tracks: HashMap<String, TrackWeak>,

	/// Pending requests by track name. Used both to fan out the resolved
	/// producer to multiple awaiting subscribers and as the queue that
	/// [`BroadcastDynamic::requested_track`] drains.
	requests: HashMap<String, PendingRequest>,

	/// Names in `requests` ordered FIFO for the dynamic handler.
	request_order: Vec<String>,

	/// The current number of dynamic producers.
	/// If this is 0, requests must be empty.
	dynamic: usize,

	/// The error that caused the broadcast to be aborted, if any.
	abort: Option<Error>,
}

fn modify(state: &conducer::Producer<State>) -> Result<conducer::Mut<'_, State>, Error> {
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

	/// Drop the named pending request and notify all resolvers with `err`.
	fn deny_request(&mut self, name: &str, err: Error) {
		if let Some(pending) = self.requests.remove(name) {
			self.request_order.retain(|n| n != name);
			for tx in pending.resolvers {
				let _ = tx.send(Err(err.clone()));
			}
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
	state: conducer::Producer<State>,
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
	pub fn insert_track(&mut self, track: TrackConsumer) -> Result<(), Error> {
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
	pub fn create_track(&mut self, track: Track) -> Result<TrackProducer, Error> {
		let track = TrackProducer::new(track);
		let mut state = modify(&self.state)?;
		state.insert_track(track.weak())?;
		drop(state);
		Ok(track)
	}

	/// Create a track with a unique name using the given suffix.
	///
	/// Generates names like `0{suffix}`, `1{suffix}`, etc. and picks the first
	/// one not already used in this broadcast.
	pub fn unique_name(&self, suffix: &str) -> String {
		let state = self.state.read();
		for i in 0u32.. {
			let name = format!("{i}{suffix}");
			if !state.tracks.contains_key(&name) {
				return name;
			}
		}
		unreachable!("u32 namespace exhausted");
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

		// Abort any pending dynamic track requests; their producers are owned
		// by the broadcast and would otherwise leave consumers stuck forever.
		for name in std::mem::take(&mut guard.request_order) {
			if let Some(pending) = guard.requests.remove(&name) {
				for tx in pending.resolvers {
					let _ = tx.send(Err(err.clone()));
				}
			}
		}

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
		self.insert_track(track.consume()).expect("should not have errored")
	}
}

/// A pending track subscription. Hold this until you have constructed the full
/// [`Track`] and call [`Self::accept`], or [`Self::deny`] if the request can't
/// be served. Dropping without calling either implicitly denies with
/// [`Error::Cancel`].
pub struct TrackRequest {
	name: String,
	subscription: Subscription,
	state: conducer::Producer<State>,
	/// `None` after [`Self::accept`] or [`Self::deny`] has been called, so the
	/// Drop impl knows not to double-deny.
	completed: bool,
}

impl TrackRequest {
	/// The track name requested by the subscriber(s).
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The first subscriber's requested [`Subscription`]. Use this as a hint
	/// for how to configure the [`Track`] (priority, timescale, etc.). Once
	/// the request is accepted, the full aggregate becomes visible via
	/// [`TrackProducer::max_priority`] / [`TrackProducer::max_timeout`].
	pub fn subscription(&self) -> &Subscription {
		&self.subscription
	}

	/// Fulfill the request with the given track. The track's `name` must match
	/// [`Self::name`]; returns [`Error::NotFound`] otherwise.
	pub fn accept(mut self, track: Track) -> Result<TrackProducer, Error> {
		if track.name != self.name {
			return Err(Error::NotFound);
		}

		let producer = TrackProducer::new(track);
		self.completed = true;

		let mut state = modify(&self.state)?;
		let pending = state.requests.remove(&self.name).ok_or(Error::Cancel)?;

		// Insert the producer's weak so future subscribe_track calls dedupe.
		state.insert_track(producer.weak()).ok();

		// Fan out a TrackConsumer to each waiting subscriber, carrying their
		// own Subscription (the first subscriber's subscription matches the
		// request; remaining resolvers in the queue already added theirs).
		let weak = producer.weak();
		let mut resolvers = pending.resolvers.into_iter();
		if let Some(tx) = resolvers.next() {
			let _ = tx.send(Ok(weak.consume_with(pending.subscription.clone())));
		}
		for tx in resolvers {
			let _ = tx.send(Ok(weak.consume_with(Subscription::default())));
		}

		// Spawn the cleanup task that removes the entry once nobody is consuming.
		let consumer_state = self.state.clone();
		let weak_for_cleanup = producer.weak();
		web_async::spawn(async move {
			let _ = weak_for_cleanup.unused().await;
			let Ok(mut state) = consumer_state.write() else { return };

			if let Some(current) = state.tracks.remove(&weak_for_cleanup.info.name)
				&& !current.is_clone(&weak_for_cleanup)
			{
				state.tracks.insert(current.info.name.clone(), current);
			}
		});

		Ok(producer)
	}

	/// Reject the request with the given error, waking all waiting subscribers.
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

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, the dynamic producer
/// can pick up the request via [`Self::requested_track`] and either
/// [`TrackRequest::accept`] it with a concrete [`Track`] or
/// [`TrackRequest::deny`] it. Dropped when no longer needed; pending requests
/// are automatically aborted.
pub struct BroadcastDynamic {
	info: Broadcast,
	state: conducer::Producer<State>,
}

impl Clone for BroadcastDynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: bump `state.dynamic` so each live handle is counted.
		// Without this, deriving Clone would let `Drop` decrement past `new`'s
		// single increment and prematurely flip `dynamic` to zero, causing
		// future `subscribe_track` calls to return `NotFound`.
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
	fn new(info: Broadcast, state: conducer::Producer<State>) -> Self {
		if let Ok(mut state) = state.write() {
			// If the broadcast is already closed, we can't handle any new requests.
			state.dynamic += 1;
		}

		Self { info, state }
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R, Error>>
	where
		F: FnMut(&mut conducer::Mut<'_, State>) -> Poll<R>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(r) => Ok(r),
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for the next consumer-requested track.
	pub fn poll_requested_track(&mut self, waiter: &conducer::Waiter) -> Poll<Result<TrackRequest, Error>> {
		let state_clone = self.state.clone();
		self.poll(waiter, |state| {
			let Some(name) = state.request_order.first().cloned() else {
				return Poll::Pending;
			};
			let pending = state.requests.get(&name).expect("request_order must mirror requests");
			let subscription = pending.subscription.clone();
			state.request_order.remove(0);
			Poll::Ready((name, subscription))
		})
		.map(|res| {
			res.map(|(name, subscription)| TrackRequest {
				name,
				subscription,
				state: state_clone,
				completed: false,
			})
		})
	}

	/// Block until a consumer requests a track, returning a request handle.
	pub async fn requested_track(&mut self) -> Result<TrackRequest, Error> {
		conducer::wait(|waiter| self.poll_requested_track(waiter)).await
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

		for name in std::mem::take(&mut guard.request_order) {
			if let Some(pending) = guard.requests.remove(&name) {
				for tx in pending.resolvers {
					let _ = tx.send(Err(err.clone()));
				}
			}
		}

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

			// Abort all pending requests since there's no dynamic producer to handle them.
			for name in std::mem::take(&mut state.request_order) {
				if let Some(pending) = state.requests.remove(&name) {
					for tx in pending.resolvers {
						let _ = tx.send(Err(Error::Cancel));
					}
				}
			}
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
	state: conducer::Consumer<State>,
}

impl Deref for BroadcastConsumer {
	type Target = Broadcast;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl BroadcastConsumer {
	/// Subscribe to a track on this broadcast.
	///
	/// Returns once the publisher has resolved the track's properties (priority
	/// and timescale) via the dynamic handler's [`TrackRequest::accept`].
	/// Reuses an existing producer if one is already publishing the track;
	/// otherwise queues a new dynamic request. Returns [`Error::NotFound`] if
	/// no dynamic producer exists to service the request.
	///
	/// The returned [`TrackConsumer`] dereferences to a [`Track`] with the
	/// publisher's authoritative properties. The subscription is tracked
	/// internally and contributes to the producer's
	/// [`TrackProducer::max_priority`] / [`TrackProducer::max_timeout`]
	/// aggregates; call [`TrackConsumer::update_subscription`] to update.
	pub async fn subscribe_track(&self, name: &str, subscription: Subscription) -> Result<TrackConsumer, Error> {
		let rx = {
			// Upgrade to a temporary producer so we can modify the state.
			let producer = self
				.state
				.produce()
				.ok_or_else(|| self.state.read().abort.clone().unwrap_or(Error::Dropped))?;
			let mut state = modify(&producer)?;

			// Reuse an existing producer if one is already live.
			if let Some(weak) = state.tracks.get(name) {
				if !weak.is_closed() {
					return Ok(weak.consume_with(subscription));
				}
				// Stale entry; remove and treat as a new request.
				state.tracks.remove(name);
			}

			let (tx, rx) = oneshot::channel();

			// Coalesce with an in-flight request for the same name.
			if let Some(pending) = state.requests.get_mut(name) {
				pending.resolvers.push(tx);
				rx
			} else if state.dynamic == 0 {
				return Err(Error::NotFound);
			} else {
				state.requests.insert(
					name.to_string(),
					PendingRequest {
						subscription: subscription.clone(),
						resolvers: vec![tx],
					},
				);
				state.request_order.push(name.to_string());
				rx
			}
		};

		match rx.await {
			Ok(res) => res,
			Err(_) => Err(self.state.read().abort.clone().unwrap_or(Error::Cancel)),
		}
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

	/// Register a [`conducer::Waiter`] that fires when the broadcast closes.
	///
	/// Returns [`Poll::Ready`] if already closed, otherwise [`Poll::Pending`] after
	/// arming the waiter. Useful for composing close-detection into a larger poll
	/// without spawning a task per broadcast.
	pub fn poll_closed(&self, waiter: &conducer::Waiter) -> Poll<()> {
		self.state.poll_closed(waiter)
	}

	/// Check if this is the exact same instance of a broadcast.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
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

	#[tokio::test]
	async fn insert() {
		let mut producer = Broadcast::new().produce();
		let mut track1 = Track::new("track1").produce();

		// Make sure we can insert before a consumer is created.
		producer.assert_insert_track(&track1);
		track1.append_group().unwrap();

		let consumer = producer.consume();

		let mut track1_sub = consumer
			.subscribe_track("track1", Subscription::default())
			.await
			.expect("should subscribe");
		track1_sub.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.assert_insert_track(&track2);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2
			.subscribe_track("track2", Subscription::default())
			.await
			.expect("should subscribe");
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

		// Create a new track and insert it into the broadcast so subscribe_track
		// can resolve immediately.
		let track1 = producer.assert_create_track(&Track::new("track1"));
		let track1c = consumer
			.subscribe_track("track1", Subscription::default())
			.await
			.expect("should subscribe");

		// Aborting the broadcast must NOT cascade to externally-owned tracks.
		producer.abort(Error::Cancel).unwrap();

		// track1's producer is held outside the broadcast, so it survives.
		assert!(!track1.is_closed());
		track1c.assert_not_closed();
	}

	#[tokio::test]
	async fn requests() {
		let mut producer = Broadcast::new().produce().dynamic();
		let consumer = producer.consume();
		let consumer2 = consumer.clone();

		// Spawn the subscriber tasks since subscribe_track now awaits resolution.
		let sub1 = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.subscribe_track("track1", Subscription::default()).await }
		});
		let sub2 = tokio::spawn({
			let consumer = consumer2.clone();
			async move { consumer.subscribe_track("track1", Subscription::default()).await }
		});

		// Give the spawned tasks a chance to register their requests.
		tokio::task::yield_now().await;
		tokio::task::yield_now().await;

		// Get the requested track, and there should only be one (deduped).
		let request = producer.assert_request();
		assert_eq!(request.name(), "track1");
		producer.assert_no_request();

		let mut track_producer = request.accept(Track::new("track1")).unwrap();

		let mut track1 = sub1.await.unwrap().expect("should resolve");
		let mut track2 = sub2.await.unwrap().expect("should resolve");
		track1.assert_is_clone(&track2);

		// Append a group and make sure both consumers receive it.
		track_producer.append_group().unwrap();
		track1.assert_group();
		track2.assert_group();

		// Subscribe to a new track and drop the dynamic. The pending sub aborts.
		let sub3 = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.subscribe_track("track2", Subscription::default()).await }
		});
		tokio::task::yield_now().await;
		drop(producer);

		assert!(sub3.await.unwrap().is_err(), "request should be cancelled");

		// Subscribing now should return NotFound (no dynamic).
		let res = consumer2.subscribe_track("track3", Subscription::default()).await;
		assert!(res.is_err(), "should have errored");
	}

	#[tokio::test]
	async fn stale_producer() {
		let mut broadcast = Broadcast::new().produce().dynamic();
		let consumer = broadcast.consume();

		// Subscribe in the background.
		let sub1 = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.subscribe_track("track1", Subscription::default()).await }
		});
		tokio::task::yield_now().await;

		let request = broadcast.assert_request();
		let mut producer1 = request.accept(Track::new("track1")).unwrap();

		let track1 = sub1.await.unwrap().expect("should resolve");

		producer1.append_group().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// Consumer should see the track as closed.
		track1.assert_closed();

		// Subscribe again to the same track. Should get a NEW producer.
		let sub2 = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.subscribe_track("track1", Subscription::default()).await }
		});
		tokio::task::yield_now().await;
		// Give the cleanup task a tick.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		tokio::task::yield_now().await;
		let request2 = broadcast.assert_request();
		let mut producer2 = request2.accept(Track::new("track1")).unwrap();
		producer2.append_group().unwrap();

		let mut track2 = sub2.await.unwrap().expect("should resolve");
		track2.assert_not_closed();
		track2.assert_not_clone(&track1);
		track2.assert_group();
	}

	// Cloning a `BroadcastDynamic` and dropping the clone must not flip
	// `state.dynamic` to zero. The relay's lite subscriber clones the
	// dynamic per spawned subscribe; if Clone skipped the increment, the
	// first finished subscribe would tear down the broadcast and any
	// follow-up `subscribe_track` would return `NotFound`.
	#[tokio::test]
	async fn dynamic_clone_keeps_alive() {
		let broadcast = Broadcast::new().produce().dynamic();
		let consumer = broadcast.consume();

		let clone = broadcast.clone();
		drop(clone);

		// Original handle is still live, so requests must still be accepted.
		let sub = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.subscribe_track("track1", Subscription::default()).await }
		});
		tokio::task::yield_now().await;

		// The request must still be there.
		let mut broadcast = broadcast;
		let request = broadcast.assert_request();
		request.accept(Track::new("track1")).unwrap();
		sub.await.unwrap().expect("should resolve");
	}
}
