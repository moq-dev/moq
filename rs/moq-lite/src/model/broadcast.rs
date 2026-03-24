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

impl State {
	/// Insert a track into the lookup, returning an error if a live track with the same name exists.
	fn insert_track(&mut self, weak: TrackWeak) -> Result<(), Error> {
		match self.tracks.entry(weak.info.name.clone()) {
			hash_map::Entry::Occupied(mut entry) => {
				if !entry.get().is_closed() {
					return Err(Error::Duplicate);
				}
				entry.insert(weak);
			}
			hash_map::Entry::Vacant(entry) => {
				entry.insert(weak);
			}
		}
		Ok(())
	}
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

	/// Insert a track consumer into the lookup, returning an error on duplicate.
	///
	/// This allows sharing a track from another broadcast without copying data.
	pub fn insert_track(&self, track: TrackConsumer) -> Result<(), Error> {
		let mut guard = modify(&self.state)?;
		guard.insert_track(track.weak())?;
		Ok(())
	}

	/// Remove a track from the lookup.
	pub fn remove_track(&self, name: &str) -> Result<(), Error> {
		let mut state = modify(&self.state)?;
		state.tracks.remove(name).ok_or(Error::UnknownTrack)?;
		Ok(())
	}

	/// Produce a new track and insert it into the broadcast.
	pub fn create_track(&self, track: Track) -> Result<TrackProducer, Error> {
		let track = TrackProducer::new(track);
		let mut guard = modify(&self.state)?;
		guard.insert_track(track.weak())?;
		drop(guard);
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
	pub fn abort(&self, err: Error) -> Result<(), Error> {
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
	pub fn assert_create_track(&self, track: &Track) -> TrackProducer {
		self.create_track(track.clone()).expect("should not have errored")
	}

	pub fn assert_insert_track(&self, track: &TrackProducer) {
		self.insert_track(track.consume()).expect("should not have errored")
	}
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
	pub fn insert_track(&self, track: TrackConsumer) -> Result<(), Error> {
		let mut guard = modify(&self.state)?;
		guard.insert_track(track.weak())?;
		Ok(())
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> BroadcastConsumer {
		BroadcastConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
		}
	}

	/// Abort the broadcast with the given error.
	pub fn abort(&self, err: Error) -> Result<(), Error> {
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
	/// Returns the track if it exists, otherwise tries to route it to [`BroadcastDynamic`].
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
			return Err(Error::UnknownTrack);
		}

		// Create a new TrackProducer, insert into lookup, and queue for dynamic handler.
		let track_producer = TrackProducer::new(track.clone());
		state.insert_track(track_producer.weak())?;
		let consumer = track_producer.consume();
		state.requested.push(track_producer);

		Ok(consumer)
	}

	/// Subscribe to a track.
	///
	/// Convenience: calls [`Self::consume_track`] then [`TrackConsumer::subscribe`].
	pub fn subscribe_track(&self, track: &Track, sub: Subscription) -> Result<TrackSubscriber, Error> {
		let consumer = self.consume_track(track)?;
		consumer.subscribe(sub)
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
		let producer = Broadcast::new().produce();
		let mut track1 = Track::new("track1").produce();

		// Make sure we can insert before a consumer is created.
		producer.assert_insert_track(&track1);
		track1.append_group().unwrap();

		let consumer = producer.consume();

		let track1_consumer = consumer.assert_consume_track(&Track::new("track1"));
		assert_eq!(track1_consumer.latest(), Some(0));

		let mut track2 = Track::new("track2").produce();
		producer.assert_insert_track(&track2);

		let consumer2 = producer.consume();
		let track2_consumer = consumer2.assert_consume_track(&Track::new("track2"));
		assert_eq!(track2_consumer.latest(), None);

		track2.append_group().unwrap();
		assert_eq!(track2_consumer.latest(), Some(0));
	}

	#[tokio::test]
	async fn closed() {
		let producer = Broadcast::new().produce();
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
		let track1_consumer = consumer.assert_consume_track(&Track::new("track1"));
		assert_eq!(track1_consumer.latest(), None);

		// Get the request -- there should be exactly one.
		let mut track1_producer = dynamic.assert_request();
		dynamic.assert_no_request();
		assert_eq!(track1_producer.info.name, "track1");

		// Dedup: consuming the same track again should return the existing one.
		let track1_dup = consumer2.assert_consume_track(&Track::new("track1"));
		track1_dup.assert_is_clone(&track1_consumer);

		// No new request should be queued.
		dynamic.assert_no_request();

		// Append a group and make sure both see it.
		track1_producer.append_group().unwrap();
		assert_eq!(track1_consumer.latest(), Some(0));
		assert_eq!(track1_dup.latest(), Some(0));
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
		drop(track1_consumer);

		// Subscribe again to the same track -- should get a new request
		// because the old producer was dropped (weak ref is closed).
		let track2_consumer = consumer.assert_consume_track(&Track::new("track1"));

		let mut producer2 = dynamic.assert_request();

		producer2.append_group().unwrap();
		assert_eq!(track2_consumer.latest(), Some(0));
	}

	#[tokio::test]
	async fn requested_unused() {
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

		// Drop the producer so the weak ref is closed.
		drop(producer1);

		// Now consume_track finds the stale entry, removes it, and creates a new request.
		let c2 = broadcast.consume();
		let _consumer3 = c2.assert_consume_track(&Track::new("unknown_track"));

		let _producer2 = dynamic.assert_request();
		dynamic.assert_no_request();
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
