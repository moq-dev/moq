use std::{
	collections::{HashMap, hash_map},
	task::{Poll, ready},
};

use std::ops::Deref;

use crate::{Error, Subscription, TrackConsumer, TrackProducer, TrackSubscriber, model::track::TrackWeak};

use super::Track;

/// A collection of media tracks that can be published and subscribed to.
///
/// Create via [`Broadcast::produce`] to obtain both [`BroadcastProducer`] and [`BroadcastConsumer`] pair.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Broadcast {
	/// The number of hops from the origin.
	pub hops: u64,
}

impl Broadcast {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn with_hops(mut self, hops: u64) -> Self {
		self.hops = hops;
		self
	}

	pub fn produce(self) -> BroadcastProducer {
		BroadcastProducer::new(self)
	}
}

#[derive(Default, Clone)]
struct State {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	tracks: HashMap<String, TrackWeak>,

	// Track producers queued by consume_track for the dynamic handler.
	requested: Vec<TrackProducer>,

	// The current number of dynamic producers.
	// If this is 0, requests will fail with NotFound.
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
	pub info: Broadcast,
	state: conducer::Producer<State>,
}

impl Default for BroadcastProducer {
	fn default() -> Self {
		Self::new(Broadcast::default())
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
		insert_track_impl(&self.state, track)
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

		// Abort any pending requested track producers.
		for mut track in guard.requested.drain(..) {
			let _ = track.abort(err.clone());
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

impl Deref for BroadcastProducer {
	type Target = Broadcast;

	fn deref(&self) -> &Self::Target {
		&self.info
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

/// Shared helper to insert a track into the broadcast lookup.
fn insert_track_impl(state: &conducer::Producer<State>, track: &TrackProducer) -> Result<(), Error> {
	let mut guard = modify(state)?;

	match guard.tracks.entry(track.info.name.clone()) {
		hash_map::Entry::Occupied(mut entry) => {
			if !entry.get().is_closed() {
				return Err(Error::Duplicate);
			}
			entry.insert(track.weak());
		}
		hash_map::Entry::Vacant(entry) => {
			entry.insert(track.weak());
		}
	}

	// Spawn cleanup task to remove the track from the lookup when unused.
	let weak = track.weak();
	let consumer_state = state.consume();
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

	Ok(())
}

/// Handles on-demand track creation for a broadcast.
///
/// When a consumer requests a track that doesn't exist, a [`TrackProducer`] is
/// created and queued. The dynamic handler receives it via [`Self::requested_track`]
/// and starts filling it with data.
#[derive(Clone)]
pub struct BroadcastDynamic {
	info: Broadcast,
	state: conducer::Producer<State>,
}

impl BroadcastDynamic {
	fn new(info: Broadcast, state: conducer::Producer<State>) -> Self {
		if let Ok(mut state) = state.write() {
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
		self.poll(waiter, |state| match state.requested.pop() {
			Some(producer) => Poll::Ready(producer),
			None => Poll::Pending,
		})
	}

	/// Block until a consumer requests a track, returning the producer.
	///
	/// The handler should start filling the [`TrackProducer`] with data
	/// (e.g., by subscribing upstream).
	pub async fn requested_track(&mut self) -> Result<TrackProducer, Error> {
		conducer::wait(|waiter| self.poll_requested_track(waiter)).await
	}

	/// Insert a track into the broadcast lookup.
	pub fn insert_track(&self, track: &TrackProducer) -> Result<(), Error> {
		insert_track_impl(&self.state, track)
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

		// Abort any pending requested track producers.
		for mut track in guard.requested.drain(..) {
			let _ = track.abort(err.clone());
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

			// Abort all pending requested tracks since there's no dynamic producer to handle them.
			for mut track in state.requested.drain(..) {
				let _ = track.abort(Error::Cancel);
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
	pub info: Broadcast,
	state: conducer::Consumer<State>,
}

impl Deref for BroadcastConsumer {
	type Target = Broadcast;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl BroadcastConsumer {
	/// Look up a track by name, returning its consumer if active.
	///
	/// If the track doesn't exist and a dynamic handler is registered,
	/// a [`TrackProducer`] is created, inserted, and queued for the handler.
	/// The returned [`TrackConsumer`] is connected to that producer.
	pub fn consume_track(&self, track: &Track) -> Result<TrackConsumer, Error> {
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

		if state.dynamic == 0 {
			return Err(Error::NotFound);
		}

		// Create a new TrackProducer, insert into lookup, and queue for dynamic handler.
		let track_producer = TrackProducer::new(track.clone());

		// Insert into the lookup so subsequent consume_track calls find it.
		match state.tracks.entry(track.name.clone()) {
			hash_map::Entry::Occupied(mut entry) => {
				entry.insert(track_producer.weak());
			}
			hash_map::Entry::Vacant(entry) => {
				entry.insert(track_producer.weak());
			}
		}

		// Spawn cleanup task.
		let weak = track_producer.weak();
		let consumer_state = producer.consume();
		web_async::spawn(async move {
			let _ = weak.unused().await;

			let Some(p) = consumer_state.produce() else {
				return;
			};
			let Ok(mut s) = p.write() else {
				return;
			};

			if let Some(current) = s.tracks.remove(&weak.info.name)
				&& !current.is_clone(&weak)
			{
				s.tracks.insert(current.info.name.clone(), current);
			}
		});

		let consumer = track_producer.consume();

		// Queue the producer for the dynamic handler.
		state.requested.push(track_producer);

		Ok(consumer)
	}

	/// Subscribe to a track, blocking until the first group exists (or finish/abort).
	///
	/// Convenience: calls [`Self::consume_track`] then [`TrackConsumer::subscribe`].
	pub async fn subscribe_track(&self, track: &Track, sub: Subscription) -> Result<TrackSubscriber, Error> {
		let consumer = self.consume_track(track)?;
		consumer.subscribe(sub).await
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

		let mut track1_consumer = consumer.assert_consume_track(&Track::new("track1"));
		track1_consumer.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.assert_insert_track(&track2);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.assert_consume_track(&Track::new("track2"));
		track2_consumer.assert_no_group();

		track2.append_group().unwrap();

		track2_consumer.assert_group();
	}

	#[tokio::test]
	async fn closed() {
		let mut producer = Broadcast::new().produce();
		let dynamic = producer.dynamic();

		let consumer = producer.consume();
		consumer.assert_not_closed();

		// Create a new track and insert it into the broadcast.
		let track1 = producer.assert_create_track(&Track::new("track1"));
		let track1c = consumer.assert_consume_track(&track1.info);

		// Explicitly aborting the broadcast should cascade to child tracks.
		drop(dynamic);
		producer.abort(Error::Cancel).unwrap();

		// track1 should be closed because close() cascades.
		track1c.assert_error();
		assert!(track1.is_closed());
	}

	#[tokio::test]
	async fn requests() {
		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();

		let consumer = broadcast.consume();
		let consumer2 = consumer.clone();

		// consume_track with dynamic handler should create a producer and queue it.
		let mut track1_consumer = consumer.assert_consume_track(&Track::new("track1"));
		track1_consumer.assert_no_group();

		// Get the request -- there should be exactly one.
		let mut track1_producer = dynamic.assert_request();
		dynamic.assert_no_request();
		assert_eq!(track1_producer.info.name, "track1");

		// Dedup: consuming the same track again should return the existing one.
		let mut track1_dup = consumer2.assert_consume_track(&Track::new("track1"));
		track1_dup.assert_is_clone(&track1_consumer);

		// No new request should be queued.
		dynamic.assert_no_request();

		// Append a group and make sure both get it.
		track1_producer.append_group().unwrap();
		track1_consumer.assert_group();
		track1_dup.assert_group();
	}

	#[tokio::test]
	async fn stale_producer() {
		tokio::time::pause();

		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();
		let consumer = broadcast.consume();

		// Subscribe to a track (creates producer via dynamic).
		let track1_consumer = consumer.assert_consume_track(&Track::new("track1"));

		// Handle the request.
		let mut producer1 = dynamic.assert_request();

		// Close the producer (simulating publisher disconnect).
		producer1.append_group().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// The consumer should see the track as closed.
		track1_consumer.assert_closed();

		// Advance time to let cleanup task run.
		tokio::time::advance(std::time::Duration::from_millis(1)).await;
		let track1_consumer_clone = consumer.assert_consume_track(&Track::new("track1"));
		drop(track1_consumer);
		drop(track1_consumer_clone);
		tokio::time::advance(std::time::Duration::from_millis(1)).await;

		// Subscribe again to the same track -- should get a new request.
		let mut track2_consumer = consumer.assert_consume_track(&Track::new("track1"));

		let mut producer2 = dynamic.assert_request();

		producer2.append_group().unwrap();
		track2_consumer.assert_group();
	}

	#[tokio::test]
	async fn requested_unused() {
		tokio::time::pause();
		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();

		// Subscribe to a track.
		let c1 = broadcast.consume();
		let consumer1 = c1.assert_consume_track(&Track::new("unknown_track"));

		// Handle the request.
		let producer1 = dynamic.assert_request();

		// The track producer should NOT be unused yet because there's a consumer.
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive.
		let consumer2 = c1.assert_consume_track(&Track::new("unknown_track"));
		consumer2.assert_is_clone(&consumer1);

		drop(consumer1);
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Drop the second consumer, now the producer should be unused.
		drop(consumer2);
		assert!(
			producer1.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);

		// Advance paused time to let the async cleanup task run.
		tokio::time::advance(std::time::Duration::from_millis(1)).await;

		// Now we can subscribe again.
		let c2 = broadcast.consume();
		let _consumer3 = c2.assert_consume_track(&Track::new("unknown_track"));

		let producer2 = dynamic.assert_request();
		assert!(!producer2.is_clone(&producer1));
	}

	#[tokio::test]
	async fn pending_requests_rejected_on_drop() {
		let broadcast = Broadcast::new().produce();
		let dynamic = broadcast.dynamic();
		let consumer = broadcast.consume();

		// consume_track creates a producer and queues it.
		let track_consumer = consumer.assert_consume_track(&Track::new("track2"));

		// Drop dynamic -- pending producers should be aborted.
		drop(dynamic);

		// Track consumer should see an error.
		track_consumer.assert_error();
	}
}
