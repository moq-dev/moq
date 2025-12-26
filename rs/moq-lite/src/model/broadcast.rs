use std::{collections::HashMap, future::Future};

use crate::{Error, Track, TrackConsumer, TrackProducer};
use tokio::sync::watch;
use web_async::Lock;

#[derive(Default)]
struct State {
	// When explicitly publishing, we hold a reference to the consumer.
	// This prevents the track from being marked as "unused".
	published: HashMap<String, TrackConsumer>,

	// When requesting, we hold a reference to the producer for dynamic tracks.
	// The track will be marked as "unused" when the last consumer is dropped.
	requested: HashMap<String, TrackProducer>,
}

/// Receive broadcast/track requests and return if we can fulfill them.
#[derive(Clone)]
pub struct BroadcastProducer {
	state: Lock<State>,

	closed: watch::Sender<Option<Result<(), Error>>>,
	requested: (
		async_channel::Sender<TrackProducer>,
		async_channel::Receiver<TrackProducer>,
	),
}

impl Default for BroadcastProducer {
	fn default() -> Self {
		Self::new()
	}
}

impl BroadcastProducer {
	pub fn new() -> Self {
		Self {
			state: Default::default(),
			closed: Default::default(),
			requested: async_channel::unbounded(),
		}
	}

	/// Return the next requested track, or None if there are no Consumers.
	pub async fn requested_track(&mut self) -> Option<TrackProducer> {
		tokio::select! {
			request = self.requested.1.recv() => request.ok(),
			_ = self.closed.closed() => None,
		}
	}

	/// Produce a new track and insert it into the broadcast.
	pub fn create_track(&mut self, track: Track) -> TrackProducer {
		let track = track.clone().produce();
		self.insert_track(track.consumer);
		track.producer
	}

	/// Insert a track into the lookup, returning true if it was unique.
	pub fn insert_track(&mut self, track: TrackConsumer) -> bool {
		let mut state = self.state.lock();
		let unique = state.published.insert(track.name.to_string(), track.clone()).is_none();
		let removed = state.requested.remove(track.name.as_ref()).is_some();

		unique && !removed
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> bool {
		let mut state = self.state.lock();
		state.published.remove(name).is_some() || state.requested.remove(name).is_some()
	}

	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			state: self.state.clone(),
			closed: self.closed.subscribe(),
			requested: self.requested.0.clone(),
		}
	}

	pub fn close(&mut self) -> Result<(), Error> {
		let mut result = Ok(());

		self.closed.send_if_modified(|closed| {
			if let Some(closed) = closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			*closed = Some(Ok(()));
			true
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		let mut result = Ok(());

		self.closed.send_if_modified(|closed| {
			if let Some(Err(err)) = closed.clone() {
				result = Err(err);
				return false;
			}

			*closed = Some(Err(err));
			true
		});

		result
	}

	/// Block until there are no more consumers.
	///
	/// A new consumer can be created by calling [Self::consume] and this will block again.
	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let closed = self.closed.clone();
		async move {
			closed.closed().await;
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.closed.same_channel(&other.closed)
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

	pub fn assert_requested(&mut self) -> TrackProducer {
		self.requested_track()
			.now_or_never()
			.expect("should not have blocked")
			.expect("should be a request")
	}

	pub fn assert_no_requested(&mut self) {
		assert!(self.requested_track().now_or_never().is_none(), "should have blocked");
	}
}

/// Subscribe to abitrary broadcast/tracks.
#[derive(Clone)]
pub struct BroadcastConsumer {
	state: Lock<State>,
	closed: watch::Receiver<Option<Result<(), Error>>>,
	requested: async_channel::Sender<TrackProducer>,
}

impl BroadcastConsumer {
	fn get(&self, track: &str) -> TrackConsumer {
		let mut state = self.state.lock();

		// Clone a published track.
		if let Some(existing) = state.published.get(track) {
			return existing.clone();
		}

		// Return any requested tracks.
		if let Some(existing) = state.requested.get(track) {
			return existing.consume();
		}

		// Use the first request as the initial info.
		// NOTE: The SubscribeOk will replace the priority/max_latency.
		// TODO Make this async, so we only return the original producer's settings.
		let mut producer = TrackProducer::new(Track {
			name: track.to_string(),
			priority: 0,
			// TODO Make None an option, meaning we have no preference.
			max_latency: std::time::Duration::from_secs(10),
		});

		let consumer = producer.consume();

		match self.requested.try_send(producer.clone()) {
			Ok(()) => {}
			Err(_) => {
				producer.abort(Error::Cancel).ok();
				return consumer;
			}
		};

		state.requested.insert(track.to_string(), producer.clone());

		let state = self.state.clone();
		let track = track.to_string();

		web_async::spawn(async move {
			producer.unused().await;
			state.lock().requested.remove(&track);
		});

		web_async::spawn(async move {});

		consumer
	}

	/// Fetches the Track over the network, using the given settings.
	///
	/// Spawns a task in the background to serve the track.
	/// [TrackConsumer::meta] can be used to update the priority/max_latency of the track.
	pub fn subscribe_track(&self, track: Track) -> TrackConsumer {
		let src = self.get(&track.name);
		let track = track.produce();

		web_async::spawn(async move {
			src.proxy(track.producer).await;
		});
		track.consumer
	}

	pub async fn closed(&self) -> Result<(), Error> {
		match self.closed.clone().wait_for(|closed| closed.is_some()).await {
			Ok(closed) => closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}

	/// Check if this is the exact same instance of a broadcast.
	///
	/// Duplicate names are allowed in the case of resumption.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.closed.same_channel(&other.closed)
	}
}

#[derive(Clone)]
struct TrackDedupe {
	// We keep a reference to producers so they don't count towards our active consumers.
	// TODO TrackConsumerWeak would be cool.
	producer: TrackProducer,

	// All of the active consumers, which we monitor to update the max latency/priority.
	consumers: Vec<TrackProducer>,
}

impl TrackDedupe {
	pub fn new(init: Track) -> Self {
		let producer = TrackProducer::new(init);

		Self {
			producer,
			consumers: Vec::new(),
		}
	}

	pub fn serve(&mut self, track: TrackProducer) {
		self.consumers.push(track.clone());
		self.update();

		let mut this = self.clone();
		let mut meta = this.producer.meta();

		// TODO: Ugh I hate tokio. We should make a non-async version.
		tokio::spawn(async move {
			loop {
				tokio::select! {
					_ = this.producer.unused() => break,
					// TODO Only prove our current value, not all N
					Some(_) = meta.next() => this.update(),
				};
			}

			this.consumers.retain(|consumer| !consumer.is_clone(&track));
			this.update();
		});
	}

	fn update(&mut self) {
		let meta = self
			.consumers
			.iter()
			.map(|consumer| consumer.meta().get())
			.reduce(|a, b| a.max(&b))
			.unwrap_or_default();
		self.producer.consume().meta().set(meta);
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
		producer.insert_track(track1.consumer);
		track1.producer.append_group();

		let consumer = producer.consume();

		let mut track1_sub = consumer.subscribe_track(&track1.producer.info);
		track1_sub.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.insert_track(track2.consumer);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.subscribe_track(&track2.producer.info);
		track2_consumer.assert_no_group();

		track2.producer.append_group();

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
		let mut track1 = Track::new("track1").produce();
		track1.producer.append_group();
		producer.insert_track(track1.consumer);

		let mut track1c = consumer.subscribe_track(&track1.producer.info);
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
		drop(track1.producer);
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
		let consumer1 = broadcast.consumer.subscribe_track(&Track::new("unknown_track"));

		// Get the requested track producer
		let producer1 = broadcast.producer.assert_request();

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive
		let consumer2 = broadcast.consumer.subscribe_track(&Track::new("unknown_track"));
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
		let consumer3 = broadcast.consumer.subscribe_track(&Track::new("unknown_track"));
		let producer2 = broadcast.producer.assert_request();

		// Drop the consumer, now the producer should be unused
		drop(consumer3);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
