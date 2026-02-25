use std::{
	collections::{HashMap, hash_map},
	task::Poll,
};

use crate::{
	Error, TrackConsumer, TrackProducer,
	model::{
		state::{Consumer, Producer},
		track::TrackWeak,
		waiter::{Waiter, waiter_fn},
	},
};

use super::Track;

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

#[derive(Default, Clone)]
struct State {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	tracks: HashMap<String, TrackWeak>,

	// Dynamic tracks that have been requested.
	requests: Vec<TrackProducer>,

	// The current number of dynamic producers.
	// If this is 0, requests must be empty.
	dynamic: usize,
}

/// Receive broadcast/track requests and return if we can fulfill them.
#[derive(Clone)]
pub struct BroadcastProducer {
	state: Producer<State>,
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
		}
	}

	/// Insert a track into the lookup, returning an error on duplicate.
	///
	/// NOTE: You probably want to [TrackProducer::clone] first to keep publishing to the track.
	pub fn insert_track(&mut self, track: &TrackProducer) -> Result<(), Error> {
		let mut state = self.state.modify()?;

		let hash_map::Entry::Vacant(entry) = state.tracks.entry(track.info.name.clone()) else {
			return Err(Error::Duplicate);
		};

		entry.insert(track.weak());

		Ok(())
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&mut self, name: &str) -> Result<(), Error> {
		let mut state = self.state.modify()?;

		state.tracks.remove(name).ok_or(Error::NotFound)?;

		Ok(())
	}

	/// Produce a new track and insert it into the broadcast.
	pub fn create_track(&mut self, track: Track) -> Result<TrackProducer, Error> {
		let track = TrackProducer::new(track);
		self.insert_track(&track)?;
		Ok(track)
	}

	pub fn dynamic(&self) -> BroadcastDynamic {
		BroadcastDynamic::new(self.state.clone())
	}

	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			state: self.state.consume(),
		}
	}

	pub fn close(&mut self, err: Error) -> Result<(), Error> {
		self.state.close(err)?;
		Ok(())
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
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

#[derive(Clone)]
pub struct BroadcastDynamic {
	state: Producer<State>,
}

impl BroadcastDynamic {
	fn new(state: Producer<State>) -> Self {
		if let Ok(mut state) = state.modify() {
			// If the broadcast is already closed, we can't handle any new requests.
			state.dynamic += 1;
		}

		Self { state }
	}

	fn poll_requested_track(&self, waiter: &Waiter) -> Poll<Result<Option<TrackProducer>, Error>> {
		self.state.poll_modify(waiter, |state| {
			if state.requests.is_empty() {
				return Poll::Pending;
			}
			Poll::Ready(state.requests.pop())
		})
	}

	pub async fn requested_track(&mut self) -> Result<Option<TrackProducer>, Error> {
		waiter_fn(move |waiter| self.poll_requested_track(waiter)).await
	}

	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			state: self.state.consume(),
		}
	}

	pub fn close(&mut self, err: Error) -> Result<(), Error> {
		self.state.close(err)?;
		Ok(())
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

impl Drop for BroadcastDynamic {
	fn drop(&mut self) {
		if let Ok(mut state) = self.state.modify() {
			// We do a saturating sub so Producer::dynamic() can avoid returning an error.
			state.dynamic = state.dynamic.saturating_sub(1);
			if state.dynamic != 0 {
				return;
			}

			// Abort all pending requests since there's no dynamic producer to handle them.
			for mut request in state.requests.drain(..) {
				request.close(Error::Cancel).ok();
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
			.expect("should be a request")
	}

	pub fn assert_no_request(&mut self) {
		assert!(self.requested_track().now_or_never().is_none(), "should have blocked");
	}
}

/// Subscribe to arbitrary broadcast/tracks.
#[derive(Clone)]
pub struct BroadcastConsumer {
	state: Consumer<State>,
}

impl BroadcastConsumer {
	pub fn subscribe_track(&self, track: &Track) -> Result<TrackConsumer, Error> {
		// Upgrade to a temporary producer so we can modify the state.
		let producer = self.state.produce()?;
		let mut state = producer.modify()?;

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
		state.tracks.insert(producer.info.name.clone(), weak.clone());
		state.requests.push(producer);

		// Remove the track from the lookup when it's unused.
		let consumer_state = self.state.clone();
		web_async::spawn(async move {
			let _ = weak.unused().await;
			if let Ok(producer) = consumer_state.produce()
				&& let Ok(mut state) = producer.modify()
				&& let Some(current) = state.tracks.remove(&weak.info.name)
				&& !current.is_clone(&weak)
			{
				state.tracks.insert(current.info.name.clone(), current);
			}
		});

		Ok(consumer)
	}

	pub async fn closed(&self) -> Error {
		self.state.closed().await
	}

	/// Check if this is the exact same instance of a broadcast.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
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
		let mut producer = BroadcastProducer::new();
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
		let mut producer = BroadcastProducer::new();
		let dynamic = producer.dynamic();

		let consumer = producer.consume();
		consumer.assert_not_closed();

		// Create a new track and insert it into the broadcast.
		let mut track1 = producer.assert_create_track(&Track::new("track1"));
		track1.append_group().unwrap();

		let mut track1c = consumer.assert_subscribe_track(&track1.info);
		let track2 = consumer.assert_subscribe_track(&Track::new("track2"));

		drop(producer);
		drop(dynamic);
		consumer.assert_closed();

		// The requested TrackProducer should have been aborted, so the track should be closed.
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
	async fn requests() {
		let mut producer = BroadcastProducer::new().dynamic();

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
		let mut broadcast = Broadcast::produce().dynamic();
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
		let mut broadcast = Broadcast::produce().dynamic();

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
