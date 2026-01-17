use std::{
	collections::{hash_map, HashMap, VecDeque},
	sync::atomic::AtomicUsize,
	task::Poll,
};

use super::{Consumer, Delivery, Produce, Producer, Track, TrackConsumer, TrackProducer};
use crate::{waiter_fn, Error, ProducerConsumer, Waiter};

/// A collection of media tracks that can be published and subscribed to.
///
/// Create via [`Broadcast::produce`] to obtain both [`BroadcastProducer`] and [`BroadcastConsumer`] pair.
#[derive(Clone, Default)]
pub struct Broadcast {
	// NOTE: Broadcasts have no names because they're often relative.
}

impl Broadcast {
	pub fn produce() -> Produce<BroadcastProducer, BroadcastConsumer> {
		let producer = BroadcastProducer::new();
		Produce {
			consumer: producer.consume(),
			producer,
		}
	}
}

#[derive(Default)]
struct State {
	producers: HashMap<String, TrackProducer>,
	requested: VecDeque<TrackProducer>,
	fin: bool,
}

impl State {
	fn poll_requested(&mut self) -> Poll<Option<TrackProducer>> {
		if let Some(track) = self.requested.pop_front() {
			return Poll::Ready(Some(track));
		}

		if self.fin {
			return Poll::Ready(None);
		}

		Poll::Pending
	}

	fn poll_requested_ready(&self) -> Poll<()> {
		if !self.requested.is_empty() {
			return Poll::Ready(());
		}

		Poll::Pending
	}
}

/// Receive broadcast/track requests and return if we can fulfill them.
#[derive(Clone, Default)]
pub struct BroadcastProducer {
	state: Producer<State>,
}

impl BroadcastProducer {
	pub fn new() -> Self {
		Self {
			producer: Producer::default(),
			consumer: ProducerConsumer::default(),
		}
	}

	/// Return the next requested track, None if closed, Error if aborted.
	pub async fn requested_track(&mut self) -> Result<Option<TrackProducer>, Error> {
		waiter_fn(move |waiter| self.poll_requested_track(waiter)).await
	}

	pub fn poll_requested_track(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Option<TrackProducer>, Error>> {
		self.state.poll_modify(waiter, |state| state.poll_requested())
	}

	/// Produce a new track and insert it into the broadcast.
	pub fn create_track<T: Into<Track>>(&mut self, track: T, delivery: Delivery) -> Result<TrackProducer, Error> {
		let track = TrackProducer::new(track.into());
		self.publish_track(track.clone())?;
		Ok(track)
	}

	/// Insert a track into the broadcast.
	pub fn publish_track(&mut self, track: TrackProducer) -> Result<(), Error> {
		let name = track.name.to_string();

		let mut state = self.state.modify()?;
		let hash_map::Entry::Vacant(entry) = state.producers.entry(name) else {
			return Err(Error::Duplicate);
		};
		entry.insert(track);
		Ok(())
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> Result<TrackProducer, Error> {
		let mut state = self.state.modify()?;
		Ok(state.producers.remove(name).ok_or(Error::NotFound)?)
	}

	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			state: self.state.consume(),
		}
	}

	pub fn close(&mut self) -> Result<(), Error> {
		self.state.modify()?.fin = true;
		Ok(())
	}

	pub fn abort(self, err: Error) -> Result<(), Error> {
		self.state.close(err)
	}

	/// Block until there are no more consumers.
	///
	/// A new consumer can be created by calling [Self::consume] and this will block again.
	pub async fn unused(&self) -> Result<(), Error> {
		self.state.unused().await
	}
}

struct BroadcastDrop {
	requested: async_channel::Receiver<TrackProducer>,
}

impl BroadcastDrop {
	pub fn new(requested: async_channel::Receiver<TrackProducer>) -> Self {
		Self { requested }
	}
}

/// Subscribe to abitrary broadcast/tracks.
#[derive(Clone)]
pub struct BroadcastConsumer {
	state: Consumer<State>,
}

impl BroadcastConsumer {
	/// Starts fetches the Track over the network, using the given settings.
	pub fn subscribe_track(&self, track: impl Into<Track>, delivery: Delivery) -> Result<TrackConsumer, Error> {
		let track = track.into();

		// If the track is already published, return it.
		if let Some(existing) = self.producer.borrow().get(track.name.as_ref()).cloned() {
			return Ok(existing.upgrade()?.subscribe(delivery));
		}

		self.consumer.modify(|state| {
			if let Some(existing) = state.get(track.name.as_ref()).cloned() {
				if let Ok(existing) = existing.upgrade() {
					return Ok(existing.subscribe(delivery));
				}
			}

			// Create a new track producer using this first request's delivery information.
			// The publisher SHOULD replace them with their own settigns on OK.
			let track = TrackProducer::new(track, delivery);

			// Need to make the consumer before sending to avoid unused.
			let consumer = track.subscribe(delivery);

			if self.requested.try_send(track.clone()).is_err() {
				return Err(Error::Cancel);
			}

			state.insert(track.name.to_string(), track.weak());

			let state = self.consumer.clone();
			web_async::spawn(async move {
				track.unused().await;
				let _ = state.modify(|state| {
					state.remove(track.name.as_ref());
				});
			});

			Ok(consumer)
		})?
	}

	pub async fn closed(&self) -> Result<(), Error> {
		self.producer.closed().await
	}

	/// Check if this is the exact same instance of a broadcast.
	///
	/// Duplicate names are allowed in the case of resumption.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.producer.is_clone(&other.producer)
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
			.expect("request would have blocked")
			.expect("no request")
	}

	pub fn assert_no_request(&mut self) {
		assert!(
			self.requested_track().now_or_never().is_none(),
			"request would not have blocked"
		);
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

	pub fn assert_subscribe(&self, track: impl Into<Track>, delivery: Delivery) -> TrackConsumer {
		self.subscribe_track(track, delivery).expect("subscribe error")
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	#[tokio::test]
	async fn insert() {
		let mut producer = BroadcastProducer::new();
		let mut track1 = TrackProducer::new("track1", Delivery::default());

		// Make sure we can publish before a consumer is created.
		producer.publish_track(track1.clone()).unwrap();
		track1.append_group().unwrap();

		let consumer = producer.consume();

		let mut track1_sub = consumer.assert_subscribe(track1.info(), Delivery::default());
		track1_sub.assert_any_group();

		let mut track2 = TrackProducer::new("track2", Delivery::default());
		producer.publish_track(track2.clone()).unwrap();

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.assert_subscribe(track2.info(), Delivery::default());
		track2_consumer.assert_no_group();

		track2.append_group().unwrap();

		track2_consumer.assert_any_group();
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

		let track1 = consumer3.subscribe_track(Track::new("track1"), Delivery::default());

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

		// Create a new track and publish it to the broadcast.
		let mut track1 = TrackProducer::new("track1", Delivery::default());
		track1.append_group().unwrap();
		producer.publish_track(track1.clone()).unwrap();

		let mut track1c = consumer.assert_subscribe("track1", Delivery::default());
		let track2 = consumer.assert_subscribe(Track::new("track2"), Delivery::default());

		drop(producer);
		consumer.assert_closed();

		// The requested TrackProducer should have been dropped, so the track should be closed.
		track2.assert_closed();

		// But track1 is still open because we currently don't cascade the closed state.
		track1c.assert_any_group();
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

		let mut track1 = consumer.assert_subscribe(Track::new("track1"), Delivery::default());
		track1.assert_not_closed();
		track1.assert_no_group();

		// Make sure we deduplicate requests while track1 is still active.
		let mut track2 = consumer2.assert_subscribe(Track::new("track1"), Delivery::default());
		track2.assert_is_clone(&track1);

		// Get the requested track, and there should only be one.
		let mut track3 = producer.assert_request();
		producer.assert_no_request();

		// Make sure the consumer is the same.
		track3.subscribe(Delivery::default()).assert_is_clone(&track1);

		// Append a group and make sure they all get it.
		track3.append_group().unwrap();
		track1.assert_any_group();
		track2.assert_any_group();

		// Make sure that tracks are cancelled when the producer is dropped.
		let track4 = consumer.assert_subscribe(Track::new("track2"), Delivery::default());
		drop(producer);

		// Make sure the track is errored, not closed.
		track4.assert_error();

		let track5 = consumer2.assert_subscribe(Track::new("track3"), Delivery::default());
		track5.assert_error();
	}

	#[tokio::test]
	async fn requested_unused() {
		let mut broadcast = Broadcast::produce();

		// Subscribe to a track that doesn't exist - this creates a request
		let consumer1 = broadcast
			.consumer
			.assert_subscribe(Track::new("unknown_track"), Delivery::default());

		// Get the requested track producer
		let producer1 = broadcast.producer.assert_request();

		// The track producer should NOT be unused yet because there's a consumer
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive
		let consumer2 = broadcast
			.consumer
			.assert_subscribe(Track::new("unknown_track"), Delivery::default());
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
			.consumer
			.subscribe_track(Track::new("unknown_track"), Delivery::default());
		let producer2 = broadcast.producer.assert_request();

		// Drop the consumer, now the producer should be unused
		drop(consumer3);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
