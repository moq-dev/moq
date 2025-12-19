use std::{collections::HashMap, future::Future};

use crate::{Error, TrackConsumer, TrackProducer};
use tokio::sync::watch;
use web_async::Lock;

use super::Track;

struct State {
	// When explicitly publishing, we hold a reference to the consumer too.
	// This prevents the track from being marked as "unused".
	published: HashMap<String, (TrackProducer, TrackConsumer)>,

	// When requesting, we hold a reference to the producer for dynamic tracks.
	// The track will be marked as "unused" when the last consumer is dropped.
	requested: HashMap<String, TrackProducer>,
}

/// Receive broadcast/track requests and return if we can fulfill them.
#[derive(Clone)]
pub struct BroadcastProducer {
	state: Lock<State>,
	closed: watch::Sender<bool>,
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
			state: Lock::new(State {
				published: HashMap::new(),
				requested: HashMap::new(),
			}),
			closed: Default::default(),
			requested: async_channel::unbounded(),
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
	pub fn insert_track(&mut self, track: TrackProducer) -> bool {
		let mut state = self.state.lock();
		let unique = state
			.published
			.insert(track.name.clone(), (track.clone(), track.consume()))
			.is_none();
		let removed = state.requested.remove(&track.name).is_some();

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

	pub fn close(&mut self) {
		self.closed.send_modify(|closed| *closed = true);
	}

	/// Block until there are no more consumers.
	///
	/// A new consumer can be created by calling [Self::consume] and this will block again.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let closed = self.closed.clone();
		async move { closed.closed().await }
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
	closed: watch::Receiver<bool>,
	requested: async_channel::Sender<TrackProducer>,
}

impl BroadcastConsumer {
	pub async fn serve(&self, track: TrackProducer) -> Result<(), Error> {
		self.get_track(track.info()).proxy(track).await
	}

	fn get_track(&self, track: &Track) -> TrackConsumer {
		let mut state = self.state.lock();

		// Return any explictly published track.
		if let Some((producer, _)) = state.published.get(&track.name) {
			return producer.consume();
		}

		// Return any requested tracks.
		if let Some(producer) = state.requested.get(&track.name) {
			return producer.consume();
		}

		// Otherwise we have never seen this track before and need to create a new producer.
		let mut producer = TrackProducer::new(track.clone());
		let consumer = producer.consume();

		// Insert the producer into the lookup so we will deduplicate requests.
		// This is not a subscriber so it doesn't count towards "used" subscribers.
		match self.requested.try_send(producer.clone()) {
			Ok(()) => {}
			Err(_) => {
				producer.abort(Error::Cancel).ok();
				return consumer;
			}
		};

		// Insert the producer into the lookup so we will deduplicate requests.
		state.requested.insert(producer.name.clone(), producer.clone());

		// Remove the track from the lookup when it's unused.
		let state = self.state.clone();
		web_async::spawn(async move {
			producer.unused().await;
			state.lock().requested.remove(&producer.name);
		});

		consumer
	}

	// Backwards compatibility for the old API.
	pub fn subscribe(&self, track: &Track) -> TrackConsumer {
		let source = self.get_track(track);
		let producer = TrackProducer::new(track.clone());
		let consumer = producer.consume();
		web_async::spawn(async move {
			if let Err(err) = source.proxy(producer).await {
				tracing::warn!(%err, "error proxying track");
			}
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
	use std::time::Duration;

	use super::*;

	#[tokio::test]
	async fn insert() {
		let mut producer = BroadcastProducer::new();
		let mut track1 = TrackProducer::new(Track {
			name: "track1".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});

		// Make sure we can insert before a consumer is created.
		producer.insert_track(track1.clone());
		track1.append_group().expect("should be able to append group");

		let consumer = producer.consume();

		let mut track1_sub = consumer.subscribe(track1.info());
		track1_sub.assert_group();

		let mut track2 = TrackProducer::new(Track {
			name: "track2".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		producer.insert_track(track2.clone());

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.subscribe(track2.info());
		track2_consumer.assert_no_group();

		track2.append_group().expect("should be able to append group");

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

		let track1 = consumer3.subscribe(&Track {
			name: "track1".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});

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
		let mut track1 = TrackProducer::new(Track {
			name: "track1".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		track1.append_group().expect("should be able to append group");
		producer.insert_track(track1.clone());

		let mut track1c = consumer.subscribe(track1.info());
		let track2 = consumer.subscribe(&Track {
			name: "track2".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});

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

		let mut track1 = consumer.subscribe(&Track {
			name: "track1".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		track1.assert_not_closed();
		track1.assert_no_group();

		// Make sure we deduplicate requests while track1 is still active.
		let mut track2 = consumer2.subscribe(&Track {
			name: "track1".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		track2.assert_is_clone(&track1);

		// Get the requested track, and there should only be one.
		let mut track3 = producer.assert_request();
		producer.assert_no_request();

		// Make sure the consumer is the same.
		track3.consume().assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().expect("should be able to append group");
		track1.assert_group();
		track2.assert_group();

		// Make sure that tracks are cancelled when the producer is dropped.
		let track4 = consumer.subscribe(&Track {
			name: "track2".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		drop(producer);

		// Make sure the track is errored, not closed.
		track4.assert_error();

		let track5 = consumer2.subscribe(&Track {
			name: "track3".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		track5.assert_error();
	}

	#[tokio::test]
	async fn requested_unused() {
		let mut broadcast = BroadcastProducer::new();

		// Subscribe to a track that doesn't exist - this creates a request
		let consumer1 = broadcast.consume().subscribe(&Track {
			name: "unknown_track".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});

		// Get the requested track producer
		let producer1 = broadcast.assert_request();

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive
		let consumer2 = broadcast.consume().subscribe(&Track {
			name: "unknown_track".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
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
		let consumer3 = broadcast.consume().subscribe(&Track {
			name: "unknown_track".to_string(),
			priority: 0,
			max_latency: Duration::from_secs(1),
		});
		let producer2 = broadcast.assert_request();

		// Drop the consumer, now the producer should be unused
		drop(consumer3);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
