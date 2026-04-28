use std::{
	collections::{HashMap, hash_map},
	ops::Deref,
	task::{Poll, ready},
};

use crate::{Error, Subscription, TrackConsumer, TrackProducer, TrackSubscriber, model::track::TrackWeak};

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
	pub fn new(info: Broadcast) -> Self {
		Self {
			info,
			state: Default::default(),
		}
	}

	/// Insert a track into the lookup, returning an error on duplicate.
	///
	/// NOTE: You probably want to [TrackProducer::clone] first to keep publishing to the track.
	pub fn insert_track(&mut self, track: &TrackProducer) -> Result<(), Error> {
		let mut state = modify(&self.state)?;

		let hash_map::Entry::Vacant(entry) = state.tracks.entry(track.name.clone()) else {
			return Err(Error::Duplicate);
		};

		entry.insert(track.weak());

		Ok(())
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
		self.insert_track(&track)?;
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

		self.create_track(Track::new(name))
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

	/// Abort the broadcast and all child tracks with the given error.
	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut guard = modify(&self.state)?;

		// Cascade abort to all child tracks.
		for weak in guard.tracks.values() {
			weak.abort(err.clone());
		}

		// Abort any pending dynamic track requests.
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
		self.insert_track(track).expect("should not have errored")
	}
}

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, a [TrackProducer] is created
/// and queued for the dynamic producer to fulfill via [Self::requested_track].
/// Dropped when no longer needed; pending requests are automatically aborted.
#[derive(Clone)]
pub struct BroadcastDynamic {
	info: Broadcast,
	state: conducer::Producer<State>,
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

	/// Abort the broadcast with the given error.
	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut guard = modify(&self.state)?;

		// Cascade abort to all child tracks.
		for weak in guard.tracks.values() {
			weak.abort(err.clone());
		}

		// Abort any pending dynamic track requests.
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
	/// Returns a fanout consumer for the given track.
	///
	/// Returns the cached track if it exists, otherwise routes a new request via
	/// [`BroadcastDynamic`]. Errors with [`Error::NotFound`] when no dynamic
	/// producer is attached.
	pub fn consume_track(&self, track: &Track) -> Result<TrackConsumer, Error> {
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

		// Remove the track from the lookup when it's unused.
		let consumer_state = self.state.clone();
		web_async::spawn(async move {
			let _ = weak.unused().await;

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

	/// Subscribe to a track with the given preferences.
	///
	/// Convenience: calls [`Self::consume_track`] then [`TrackConsumer::subscribe`].
	pub fn subscribe_track(&self, track: &Track, sub: Subscription) -> Result<TrackSubscriber, Error> {
		self.consume_track(track)?.subscribe(sub)
	}

	/// Convenience: [`Self::subscribe_track`] with [`Subscription::default`].
	pub fn subscribe_track_default(&self, track: &Track) -> Result<TrackSubscriber, Error> {
		self.consume_track(track)?.subscribe_default()
	}

	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}

	/// Check if this is the exact same instance of a broadcast.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

#[cfg(test)]
impl BroadcastConsumer {
	pub fn assert_consume_track(&self, track: &Track) -> TrackConsumer {
		self.consume_track(track).expect("should not have errored")
	}

	pub fn assert_subscribe_track(&self, track: &Track) -> TrackSubscriber {
		self.subscribe_track(track, Subscription::default())
			.expect("should not have errored")
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

		// Explicitly aborting the broadcast should cascade to child tracks.
		producer.abort(Error::Cancel).unwrap();

		// The requested TrackProducer should have been aborted.
		track2.assert_error();

		// track1 should also be closed because close() cascades.
		track1c.assert_error();

		// track1's producer should also be closed.
		assert!(track1.is_closed());
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

		// Append a group and make sure they all get it.
		track3.append_group().unwrap();
		track1.assert_group();
		track2.assert_group();

		// Make sure that tracks are cancelled when the producer is dropped.
		let track4 = consumer.assert_subscribe_track(&Track::new("track2"));
		drop(producer);

		// Make sure the track is errored, not closed.
		track4.assert_error();

		let track5 = consumer2.subscribe_track(&Track::new("track3"), Subscription::default());
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

	#[tokio::test]
	async fn requested_unused() {
		let mut broadcast = Broadcast::new().produce().dynamic();

		// Subscribe to a track that doesn't exist - this creates a request
		let consumer1 = broadcast.consume().assert_subscribe_track(&Track::new("unknown_track"));

		// Get the requested track producer
		let producer1 = broadcast.assert_request();

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive
		let consumer2 = broadcast.consume().assert_subscribe_track(&Track::new("unknown_track"));
		consumer2.assert_is_clone(&consumer1);

		// Drop the consumer subscription
		drop(consumer1);

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Drop the second consumer, now the producer should be unused
		drop(consumer2);

		// BUG: The track producer should become unused after dropping the consumer,
		// but it won't because the broadcast keeps a reference in the lookup HashMap
		// This assertion will fail, demonstrating the bug
		assert!(
			producer1.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);

		// TODO Unfortunately, we need to sleep for a little bit to detect when unused.
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		// Now the cleanup task should have run and we can subscribe again to the unknown track.
		let consumer3 = broadcast
			.consume()
			.subscribe_track(&Track::new("unknown_track"), Subscription::default());
		let producer2 = broadcast.assert_request();

		// Drop the consumer, now the producer should be unused
		drop(consumer3);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
