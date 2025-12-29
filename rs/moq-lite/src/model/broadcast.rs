use std::{collections::HashMap, future::Future};

use crate::{Error, Produce, Track, TrackConsumer, TrackProducer};
use tokio::sync::watch;
use web_async::Lock;

#[derive(Default)]
struct State {
	producers: HashMap<String, TrackProducer>,

	// Only when explicitly publishing a track will we hold a reference to the consumer.
	// This prevents it from being marked as "unused".
	consumers: HashMap<String, TrackConsumer>,
}

pub struct Broadcast {}

impl Broadcast {
	pub fn produce() -> Produce<BroadcastProducer, BroadcastConsumer> {
		let producer = BroadcastProducer::new();
		Produce {
			consumer: producer.consume(),
			producer,
		}
	}
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
	pub fn create_track<T: Into<Track>>(&mut self, track: T) -> TrackProducer {
		let track = TrackProducer::new(track.into());
		self.publish_track(track.clone());
		track
	}

	/// Insert a track into the broadcast.
	pub fn publish_track(&mut self, track: TrackProducer) {
		let name = track.name.to_string();

		let mut state = self.state.lock();
		state.consumers.insert(name.clone(), track.consume());
		state.producers.insert(name, track);
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) {
		let mut state = self.state.lock();
		state.consumers.remove(name);
		state.producers.remove(name);
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
	closed: watch::Receiver<Option<Result<(), Error>>>,
	requested: async_channel::Sender<TrackProducer>,
}

impl BroadcastConsumer {
	/// Fetches the Track over the network, using the given settings.
	///
	/// [TrackConsumer::meta] can be used to update the priority/max_latency of the track.
	pub fn subscribe_track<T: Into<Track>>(&self, track: T) -> TrackConsumer {
		let track = track.into();
		let mut state = self.state.lock();

		// If the track is already published, return it.
		if let Some(existing) = state.producers.get(&track.name) {
			return existing.consume();
		}

		let mut track = track.produce();

		// TODO await this
		match self.requested.try_send(track.producer.clone()) {
			Ok(()) => {}
			Err(_) => {
				track.producer.abort(Error::Cancel).ok();
				return track.consumer;
			}
		};

		state
			.producers
			.insert(track.producer.name.to_string(), track.producer.clone());
		let state = self.state.clone();

		web_async::spawn(async move {
			track.producer.unused().await;
			state.lock().producers.remove(track.producer.name.as_ref());
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

		let track1 = consumer3.subscribe_track(Track::new("track1"));

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
		let mut track1 = producer.create_track("track1");
		track1.append_group().expect("failed to append group");

		let mut track1c = consumer.subscribe_track("track1");
		let track2 = consumer.subscribe_track("track2");

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

		let mut track1 = consumer.subscribe_track("track1");
		track1.assert_not_closed();
		track1.assert_no_group();

		// Make sure we deduplicate requests while track1 is still active.
		let mut track2 = consumer2.subscribe_track("track1");
		track2.assert_is_clone(&track1);

		// Get the requested track, and there should only be one.
		let mut track3 = producer.assert_request();
		producer.assert_no_request();

		// Make sure the consumer is the same.
		track3.consume().assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().expect("failed to append group");
		track1.assert_group();
		track2.assert_group();

		// Make sure that tracks are cancelled when the producer is dropped.
		let track4 = consumer.subscribe_track("track2");
		drop(producer);

		// Make sure the track is errored, not closed.
		track4.assert_error();

		let track5 = consumer2.subscribe_track("track3");
		track5.assert_error();
	}

	#[tokio::test]
	async fn requested_unused() {
		let mut broadcast = Broadcast::produce();

		// Subscribe to a track that doesn't exist - this creates a request
		let consumer1 = broadcast.consumer.subscribe_track("unknown_track");

		// Get the requested track producer
		let producer1 = broadcast.producer.assert_request();

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive
		let consumer2 = broadcast.consumer.subscribe_track("unknown_track");
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
		let consumer3 = broadcast.consumer.subscribe_track("unknown_track");
		let producer2 = broadcast.producer.assert_request();

		// Drop the consumer, now the producer should be unused
		drop(consumer3);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
