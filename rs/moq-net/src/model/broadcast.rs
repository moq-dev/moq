use std::{
	collections::{HashMap, hash_map},
	ops::Deref,
	task::{Poll, ready},
};

use crate::{Error, TrackConsumer, TrackProducer, model::track::TrackWeak};

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

#[derive(Default, Clone)]
struct State {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	tracks: HashMap<String, TrackWeak>,

	// Dynamic tracks that have been requested.
	requests: Vec<TrackProducer>,

	// The current number of dynamic producers.
	// If this is 0, requests must be empty.
	dynamic: usize,

	// The error that caused the broadcast to be aborted, if any.
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
	pub fn unique_track(&mut self, suffix: &str) -> Result<TrackProducer, Error> {
		let state = self.state.read();
		let mut name = String::new();
		for i in 0u32.. {
			name = format!("{i}{suffix}");
			if !state.tracks.contains_key(&name) {
				break;
			}
		}
		drop(state);

		self.create_track(Track {
			name,
			..Default::default()
		})
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
		for mut request in guard.requests.drain(..) {
			request.abort(err.clone()).ok();
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

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, a [TrackProducer] is created
/// and queued for the dynamic producer to fulfill via [Self::requested_track].
/// Dropped when no longer needed; pending requests are automatically aborted.
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

	/// Poll for the next consumer-requested track, without blocking. The returned producer
	/// is preconfigured with the requested track's name and priority.
	pub fn poll_requested_track(&mut self, waiter: &conducer::Waiter) -> Poll<Result<TrackProducer, Error>> {
		self.poll(waiter, |state| match state.requests.pop() {
			Some(producer) => Poll::Ready(producer),
			None => Poll::Pending,
		})
	}

	/// Block until a consumer requests a track, returning its producer.
	pub async fn requested_track(&mut self) -> Result<TrackProducer, Error> {
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

		// Abort any pending dynamic track requests; their producers are owned
		// by the broadcast and would otherwise leave consumers stuck forever.
		for mut request in guard.requests.drain(..) {
			request.abort(err.clone()).ok();
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
			for mut request in state.requests.drain(..) {
				request.abort(Error::Cancel).ok();
			}
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl BroadcastDynamic {
	pub fn assert_request(&mut self) -> TrackProducer {
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
	/// Reuses an existing producer if one is already publishing the track; otherwise
	/// queues a new dynamic request that the broadcast's producer will service via
	/// [`BroadcastDynamic::requested_track`]. Returns [`Error::NotFound`] if the
	/// broadcast has no dynamic producer to handle requests.
	pub fn subscribe_track(&self, track: &Track) -> Result<TrackConsumer, Error> {
		// Upgrade to a temporary producer so we can modify the state.
		let producer = self
			.state
			.produce()
			.ok_or_else(|| self.state.read().abort.clone().unwrap_or(Error::Dropped))?;
		let mut state = modify(&producer)?;

		if let Some(weak) = state.tracks.get(&track.name) {
			if !weak.is_closed() {
				return Ok(weak.consume());
			}
			// Remove the stale entry
			state.tracks.remove(&track.name);
		}

		// Otherwise we have never seen this track before and need to create a new producer.
		let producer = track.clone().produce();
		let consumer = producer.consume();

		if state.dynamic == 0 {
			return Err(Error::NotFound);
		}

		// Insert a weak reference for deduplication.
		let weak = producer.weak();
		state.tracks.insert(producer.name.clone(), weak.clone());
		state.requests.push(producer);

		// Remove the track from the lookup once the producer closes. The
		// producer owner is responsible for closing it (via abort/finish/drop)
		// when truly done with the track; this lets a producer linger across
		// transient consumer churn (relay subscription churn) without losing
		// the lookup entry to a race with auto-cleanup.
		let consumer_state = self.state.clone();
		web_async::spawn(async move {
			weak.closed().await;

			let Some(producer) = consumer_state.produce() else {
				return;
			};
			let Ok(mut state) = producer.write() else {
				return;
			};

			// Remove the entry, but reinsert if it was replaced by a different reference.
			if let Some(current) = state.tracks.remove(&weak.info.name)
				&& !current.is_clone(&weak)
			{
				state.tracks.insert(current.info.name.clone(), current);
			}
		});

		Ok(consumer)
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
	pub fn assert_subscribe_track(&self, track: &Track) -> TrackConsumer {
		self.subscribe_track(track).expect("should not have errored")
	}

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

		let mut track1_sub = consumer.assert_subscribe_track(&Track::new("track1"));
		track1_sub.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.assert_insert_track(&track2);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.assert_subscribe_track(&Track::new("track2"));
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

		// Create a new track and insert it into the broadcast.
		let track1 = producer.assert_create_track(&Track::new("track1"));
		let track1c = consumer.assert_subscribe_track(&track1);
		let track2 = consumer.assert_subscribe_track(&Track::new("track2"));

		// Aborting the broadcast must NOT cascade to externally-owned tracks.
		producer.abort(Error::Cancel).unwrap();

		// track2's producer was owned by the broadcast (a pending dynamic
		// request), so the consumer surfaces the abort.
		track2.assert_error();

		// track1's producer is held outside the broadcast, so it survives.
		assert!(!track1.is_closed());
		track1c.assert_not_closed();
	}

	#[tokio::test]
	async fn requests() {
		let mut producer = Broadcast::new().produce().dynamic();

		let consumer = producer.consume();
		let consumer2 = consumer.clone();

		let mut track1 = consumer.assert_subscribe_track(&Track::new("track1"));
		track1.assert_not_closed();
		track1.assert_no_group();

		// Make sure we deduplicate requests while track1 is still active.
		let mut track2 = consumer2.assert_subscribe_track(&Track::new("track1"));
		track2.assert_is_clone(&track1);

		// Get the requested track, and there should only be one.
		let mut track3 = producer.assert_request();
		producer.assert_no_request();

		// Make sure the consumer is the same.
		track3.consume().assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().unwrap();
		track1.assert_group();
		track2.assert_group();

		// Make sure that tracks are cancelled when the producer is dropped.
		let track4 = consumer.assert_subscribe_track(&Track::new("track2"));
		drop(producer);

		// Make sure the track is errored, not closed.
		track4.assert_error();

		let track5 = consumer2.subscribe_track(&Track::new("track3"));
		assert!(track5.is_err(), "should have errored");
	}

	#[tokio::test]
	async fn stale_producer() {
		let mut broadcast = Broadcast::new().produce().dynamic();
		let consumer = broadcast.consume();

		// Subscribe to a track, creating a request
		let track1 = consumer.assert_subscribe_track(&Track::new("track1"));

		// Get the requested producer and close it (simulating publisher disconnect)
		let mut producer1 = broadcast.assert_request();
		producer1.append_group().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// The consumer should see the track as closed
		track1.assert_closed();

		// Subscribe again to the same track - should get a NEW producer, not the stale one
		let mut track2 = consumer.assert_subscribe_track(&Track::new("track1"));
		track2.assert_not_closed();
		track2.assert_not_clone(&track1);

		// There should be a new request for the track
		let mut producer2 = broadcast.assert_request();
		producer2.append_group().unwrap();

		// The new consumer should receive the new group
		track2.assert_group();
	}

	#[tokio::test(start_paused = true)]
	async fn requested_unused() {
		let mut broadcast = Broadcast::new().produce().dynamic();

		// Subscribe to a track that doesn't exist - this creates a request.
		let consumer1 = broadcast.consume().assert_subscribe_track(&Track::new("unknown_track"));
		let mut producer1 = broadcast.assert_request();
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// A second subscriber reuses the existing producer (deduplication).
		let consumer2 = broadcast.consume().assert_subscribe_track(&Track::new("unknown_track"));
		consumer2.assert_is_clone(&consumer1);

		drop(consumer2);
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		drop(consumer1);
		assert!(
			producer1.unused().now_or_never().is_some(),
			"track producer should be unused after all consumers are dropped"
		);

		// While the producer is alive, re-subscribing to the same name reuses
		// the existing producer (no new request) — this is what lets the relay
		// linger upstream subscriptions across transient consumer churn.
		let consumer3 = broadcast.consume().assert_subscribe_track(&Track::new("unknown_track"));
		consumer3.assert_is_clone(&producer1.consume());
		broadcast.assert_no_request();
		drop(consumer3);

		// Aborting the producer triggers cleanup; the next subscribe creates a fresh request.
		producer1.abort(Error::Cancel).unwrap();
		// Yield to give the spawned cleanup task a chance to run before we
		// expect its effects below. Using paused time keeps the test
		// independent of wall-clock scheduling.
		tokio::time::advance(std::time::Duration::from_millis(1)).await;

		let consumer4 = broadcast.consume().assert_subscribe_track(&Track::new("unknown_track"));
		let producer2 = broadcast.assert_request();
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
	// follow-up `subscribe_track` would return `NotFound`.
	#[tokio::test]
	async fn dynamic_clone_keeps_alive() {
		let broadcast = Broadcast::new().produce().dynamic();
		let consumer = broadcast.consume();

		let clone = broadcast.clone();
		drop(clone);

		// Original handle is still live, so requests must still be accepted.
		consumer.assert_subscribe_track(&Track::new("track1"));
	}
}
