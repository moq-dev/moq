use std::{
	collections::HashMap,
	future::Future,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use crate::{Error, TrackConsumer, TrackProducer};
use tokio::sync::watch;
use web_async::Lock;

use super::Track;

struct State {
	// When explicitly publishing, we hold a reference to the consumer.
	// This prevents the track from being marked as "unused".
	consumers: HashMap<String, TrackConsumer>,

	// When requesting, we hold a reference to the producer for dynamic tracks.
	// The track will be marked as "unused" when the last consumer is dropped.
	producers: HashMap<String, TrackProducer>,
}

/// A collection of media tracks that can be published and subscribed to.
///
/// Create via [`Broadcast::produce`] to obtain both [`BroadcastProducer`] and [`BroadcastConsumer`] pair.
#[derive(Clone, Default)]
pub struct Broadcast {
	// NOTE: Broadcasts have no names because they're often relative.
}

impl Broadcast {
	pub fn produce() -> BroadcastProducer {
		BroadcastProducer::new()
	}
}

/// Receive broadcast/track requests and return if we can fulfill them.
pub struct BroadcastProducer {
	state: Lock<State>,
	closed: watch::Sender<bool>,
	requested: (
		async_channel::Sender<TrackProducer>,
		async_channel::Receiver<TrackProducer>,
	),
	cloned: Arc<AtomicUsize>,
}

impl Default for BroadcastProducer {
	fn default() -> Self {
		Self::new()
	}
}

impl BroadcastProducer {
	pub fn new() -> Self {
		Self {
			state: Lock::new(State {
				consumers: HashMap::new(),
				producers: HashMap::new(),
			}),
			closed: Default::default(),
			requested: async_channel::unbounded(),
			cloned: Default::default(),
		}
	}

	/// Return the next requested track.
	pub async fn requested_track(&mut self) -> Option<TrackProducer> {
		self.requested.1.recv().await.ok()
	}

	/// Produce a new track and insert it into the broadcast.
	pub fn create_track(&mut self, track: Track) -> TrackProducer {
		let track = TrackProducer::new(track);
		self.insert_track(track.clone());
		track
	}

	/// Insert a track into the lookup, returning true if it was unique.
	///
	/// NOTE: You probably want to [TrackProducer::clone] to keep publishing to the track.
	pub fn insert_track(&mut self, track: TrackProducer) -> bool {
		let mut state = self.state.lock();
		state.consumers.insert(track.info.name.clone(), track.consume());
		state.producers.insert(track.info.name.clone(), track).is_none()
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> bool {
		let mut state = self.state.lock();
		state.consumers.remove(name).is_some() || state.producers.remove(name).is_some()
	}

	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			state: self.state.clone(),
			closed: self.closed.subscribe(),
			requested: self.requested.0.clone(),
		}
	}

	pub fn close(&mut self) {
		self.closed.send_modify(|closed| *closed = true);
	}

	/// Block until there are no more consumers.
	///
	/// A new consumer can be created by calling [Self::consume] and this will block again.
	pub fn unused(&self) -> impl Future<Output = ()> + use<> {
		let closed = self.closed.clone();
		async move { closed.closed().await }
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.closed.same_channel(&other.closed)
	}
}

impl Clone for BroadcastProducer {
	fn clone(&self) -> Self {
		self.cloned.fetch_add(1, Ordering::Relaxed);
		Self {
			state: self.state.clone(),
			closed: self.closed.clone(),
			requested: self.requested.clone(),
			cloned: self.cloned.clone(),
		}
	}
}

impl Drop for BroadcastProducer {
	fn drop(&mut self) {
		if self.cloned.fetch_sub(1, Ordering::Relaxed) > 0 {
			return;
		}

		// Cleanup any lingering state when the last producer is dropped.

		// Close the sender so consumers can't send any more requests.
		self.requested.0.close();

		// Drain any remaining requests.
		while let Ok(producer) = self.requested.1.try_recv() {
			producer.abort(Error::Cancel);
		}

		let mut state = self.state.lock();

		// Cleanup any published tracks.
		state.consumers.clear();
		state.producers.clear();
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl BroadcastProducer {
	pub fn assert_used(&self) {
		assert!(self.unused().now_or_never().is_none(), "should be used");
	}

	pub fn assert_unused(&self) {
		assert!(self.unused().now_or_never().is_some(), "should be unused");
	}

	pub fn assert_request(&mut self) -> TrackProducer {
		self.requested_track()
			.now_or_never()
			.expect("should not have blocked")
			.expect("should be a request")
	}

	pub fn assert_no_request(&mut self) {
		assert!(self.requested_track().now_or_never().is_none(), "should have blocked");
	}
}

/// Subscribe to abitrary broadcast/tracks.
#[derive(Clone)]
pub struct BroadcastConsumer {
	state: Lock<State>,
	closed: watch::Receiver<bool>,
	requested: async_channel::Sender<TrackProducer>,
}

impl BroadcastConsumer {
	pub fn subscribe_track(&self, track: &Track) -> TrackConsumer {
		let mut state = self.state.lock();

		if let Some(producer) = state.producers.get(&track.name) {
			return producer.consume();
		}

		// Otherwise we have never seen this track before and need to create a new producer.
		let producer = track.clone().produce();
		let consumer = producer.consume();

		// Insert the producer into the lookup so we will deduplicate requests.
		// This is not a subscriber so it doesn't count towards "used" subscribers.
		match self.requested.try_send(producer.clone()) {
			Ok(()) => {}
			Err(_) => {
				// If the BroadcastProducer is closed, immediately close the track.
				// This is a bit more ergonomic than returning None.
				producer.abort(Error::Cancel);
				return consumer;
			}
		}

		// Insert the producer into the lookup so we will deduplicate requests.
		state.producers.insert(producer.info.name.clone(), producer.clone());

		// Remove the track from the lookup when it's unused.
		let state = self.state.clone();
		web_async::spawn(async move {
			producer.unused().await;
			state.lock().producers.remove(&producer.info.name);
		});

		consumer
	}

	pub fn closed(&self) -> impl Future<Output = ()> {
		// A hacky way to check if the broadcast is closed.
		let mut closed = self.closed.clone();
		async move {
			closed.wait_for(|closed| *closed).await.ok();
		}
	}

	/// Check if this is the exact same instance of a broadcast.
	///
	/// Duplicate names are allowed in the case of resumption.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.closed.same_channel(&other.closed)
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
		let mut producer = BroadcastProducer::new();
		let mut track1 = Track::new("track1").produce();

		// Make sure we can insert before a consumer is created.
		producer.insert_track(track1.clone());
		track1.append_group();

		let consumer = producer.consume();

		let mut track1_sub = consumer.subscribe_track(&Track::new("track1"));
		track1_sub.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.insert_track(track2.clone());

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.subscribe_track(&Track::new("track2"));
		track2_consumer.assert_no_group();

		track2.append_group();

		track2_consumer.assert_group();
	}

	#[tokio::test]
	async fn unused() {
		let producer = BroadcastProducer::new();
		producer.assert_unused();

		// Create a new consumer.
		let consumer1 = producer.consume();
		producer.assert_used();

		// It's also valid to clone the consumer.
		let consumer2 = consumer1.clone();
		producer.assert_used();

		// Dropping one consumer doesn't make it unused.
		drop(consumer1);
		producer.assert_used();

		drop(consumer2);
		producer.assert_unused();

		// Even though it's unused, we can still create a new consumer.
		let consumer3 = producer.consume();
		producer.assert_used();

		let track1 = consumer3.subscribe_track(&Track::new("track1"));

		// It doesn't matter if a subscription is alive, we only care about the broadcast handle.
		// TODO is this the right behavior?
		drop(consumer3);
		producer.assert_unused();

		drop(track1);
	}

	#[tokio::test]
	async fn closed() {
		let mut producer = BroadcastProducer::new();

		let consumer = producer.consume();
		consumer.assert_not_closed();

		// Create a new track and insert it into the broadcast.
		let mut track1 = producer.create_track(Track::new("track1"));
		track1.append_group();

		let mut track1c = consumer.subscribe_track(&track1.info);
		let track2 = consumer.subscribe_track(&Track::new("track2"));

		drop(producer);
		consumer.assert_closed();

		// The requested TrackProducer should have been dropped, so the track should be closed.
		track2.assert_closed();

		// But track1 is still open because we currently don't cascade the closed state.
		track1c.assert_group();
		track1c.assert_no_group();
		track1c.assert_not_closed();

		// TODO: We should probably cascade the closed state.
		drop(track1);
		track1c.assert_closed();
	}

	#[tokio::test]
	async fn select() {
		let mut producer = BroadcastProducer::new();

		// Make sure this compiles; it's actually more involved than it should be.
		tokio::select! {
			_ = producer.unused() => {}
			_ = producer.requested_track() => {}
		}
	}

	#[tokio::test]
	async fn requests() {
		let mut producer = BroadcastProducer::new();

		let consumer = producer.consume();
		let consumer2 = consumer.clone();

		let mut track1 = consumer.subscribe_track(&Track::new("track1"));
		track1.assert_not_closed();
		track1.assert_no_group();

		// Make sure we deduplicate requests while track1 is still active.
		let mut track2 = consumer2.subscribe_track(&Track::new("track1"));
		track2.assert_is_clone(&track1);

		// Get the requested track, and there should only be one.
		let mut track3 = producer.assert_request();
		producer.assert_no_request();

		// Make sure the consumer is the same.
		track3.consume().assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group();
		track1.assert_group();
		track2.assert_group();

		// Make sure that tracks are cancelled when the producer is dropped.
		let track4 = consumer.subscribe_track(&Track::new("track2"));
		drop(producer);

		// Make sure the track is errored, not closed.
		track4.assert_error();

		let track5 = consumer2.subscribe_track(&Track::new("track3"));
		track5.assert_error();
	}

	#[tokio::test]
	async fn requested_unused() {
		let mut broadcast = Broadcast::produce();

		// Subscribe to a track that doesn't exist - this creates a request
		let consumer1 = broadcast.consume().subscribe_track(&Track::new("unknown_track"));

		// Get the requested track producer
		let producer1 = broadcast.assert_request();

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive
		let consumer2 = broadcast.consume().subscribe_track(&Track::new("unknown_track"));
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
		let consumer3 = broadcast.consume().subscribe_track(&Track::new("unknown_track"));
		let producer2 = broadcast.assert_request();

		// Drop the consumer, now the producer should be unused
		drop(consumer3);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
