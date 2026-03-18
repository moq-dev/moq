use std::{
	collections::{HashMap, hash_map},
	task::{Poll, ready},
};

use std::ops::Deref;

use crate::{Error, TrackConsumer, TrackProducer, model::track::TrackWeak};

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

/// A pending track request queued by [`BroadcastConsumer::subscribe_track`].
///
/// The dynamic handler receives this via [`BroadcastDynamic::requested_track`],
/// creates the track, inserts it, and responds.
pub struct TrackRequest {
	pub info: Track,
	replies: Vec<tokio::sync::oneshot::Sender<Result<TrackConsumer, Error>>>,
}

impl TrackRequest {
	/// Respond to all waiting subscribers with the given consumer.
	pub fn respond(mut self, consumer: TrackConsumer) {
		for tx in self.replies.drain(..) {
			let _ = tx.send(Ok(consumer.clone()));
		}
	}

	/// Reject all waiting subscribers with the given error.
	pub fn reject(mut self, err: Error) {
		for tx in self.replies.drain(..) {
			let _ = tx.send(Err(err.clone()));
		}
	}
}

impl Drop for TrackRequest {
	fn drop(&mut self) {
		// If the request is dropped without responding, reject with Cancel.
		for tx in self.replies.drain(..) {
			let _ = tx.send(Err(Error::Cancel));
		}
	}
}

#[derive(Default, Clone)]
struct State {
	// Weak references for deduplication. Doesn't prevent track auto-close.
	tracks: HashMap<String, TrackWeak>,

	// Dynamic tracks that have been requested but not yet handled.
	requests: Vec<TrackRequest>,

	// The current number of dynamic producers.
	// If this is 0, requests must be empty.
	dynamic: usize,

	// The error that caused the broadcast to be aborted, if any.
	abort: Option<Error>,
}

// TrackRequest contains oneshot senders which aren't Clone, but State derives Clone.
// We need a manual Clone that drops the requests (they can't be cloned).
// Actually, this is already handled because State's Clone impl will try to clone requests.
// Let me remove the derive and implement Clone manually.
impl Clone for TrackRequest {
	fn clone(&self) -> Self {
		// Requests can't actually be meaningfully cloned; the senders are consumed.
		// This is only needed because State derives Clone (for conducer).
		Self {
			info: self.info.clone(),
			replies: Vec::new(),
		}
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

		// Reject any pending dynamic track requests.
		for request in guard.requests.drain(..) {
			request.reject(err.clone());
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
/// When a consumer requests a track that doesn't exist, a [`TrackRequest`] is queued
/// for the dynamic producer to fulfill via [`Self::requested_track`].
/// Dropped when no longer needed; pending requests are automatically rejected.
#[derive(Clone)]
pub struct BroadcastDynamic {
	info: Broadcast,
	state: conducer::Producer<State>,
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

	pub fn poll_requested_track(&mut self, waiter: &conducer::Waiter) -> Poll<Result<TrackRequest, Error>> {
		self.poll(waiter, |state| match state.requests.pop() {
			Some(request) => Poll::Ready(request),
			None => Poll::Pending,
		})
	}

	/// Block until a consumer requests a track, returning the request.
	///
	/// The handler should create a [`TrackProducer`], insert it via [`Self::insert_track`],
	/// and then call [`TrackRequest::respond`] with the consumer.
	pub async fn requested_track(&mut self) -> Result<TrackRequest, Error> {
		conducer::wait(|waiter| self.poll_requested_track(waiter)).await
	}

	/// Insert a track into the broadcast lookup.
	///
	/// Call this before responding to a [`TrackRequest`] so that subsequent
	/// [`BroadcastConsumer::subscribe_track`] calls find the track immediately.
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

		// Reject any pending dynamic track requests.
		for request in guard.requests.drain(..) {
			request.reject(err.clone());
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

			// Reject all pending requests since there's no dynamic producer to handle them.
			for request in state.requests.drain(..) {
				request.reject(Error::Cancel);
			}
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl BroadcastDynamic {
	pub fn assert_request(&mut self) -> TrackRequest {
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
	/// Subscribe to a track by name.
	///
	/// If the track already exists in the lookup and is still active, returns its consumer
	/// immediately. Otherwise, queues a [`TrackRequest`] for the dynamic handler and awaits
	/// its response.
	pub async fn subscribe_track(&self, track: &Track) -> Result<TrackConsumer, Error> {
		let rx = {
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

			if state.dynamic == 0 {
				return Err(Error::NotFound);
			}

			let (tx, rx) = tokio::sync::oneshot::channel();

			// Dedup: if there's already a pending request for this track, piggyback.
			if let Some(existing) = state.requests.iter_mut().find(|r| r.info.name == track.name) {
				existing.replies.push(tx);
			} else {
				state.requests.push(TrackRequest {
					info: track.clone(),
					replies: vec![tx],
				});
			}

			rx
		};

		rx.await.map_err(|_| Error::Dropped)?
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
	pub async fn assert_subscribe_track(&self, track: &Track) -> TrackConsumer {
		self.subscribe_track(track).await.expect("should not have errored")
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

		let mut track1_sub = consumer.assert_subscribe_track(&Track::new("track1")).await;
		track1_sub.assert_group();

		let mut track2 = Track::new("track2").produce();
		producer.assert_insert_track(&track2);

		let consumer2 = producer.consume();
		let mut track2_consumer = consumer2.assert_subscribe_track(&Track::new("track2")).await;
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
		let track1c = consumer.assert_subscribe_track(&track1.info).await;

		// Subscribe to a dynamic track -- this will pend until the handler responds.
		let consumer2 = consumer.clone();
		let track2_handle = tokio::spawn(async move { consumer2.subscribe_track(&Track::new("track2")).await });

		// Explicitly aborting the broadcast should cascade to child tracks.
		drop(dynamic);
		producer.abort(Error::Cancel).unwrap();

		// track1 should be closed because close() cascades.
		track1c.assert_error();
		assert!(track1.is_closed());

		// track2 should have been rejected.
		let track2_result = track2_handle.await.unwrap();
		assert!(track2_result.is_err());
	}

	#[tokio::test]
	async fn requests() {
		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();

		let consumer = broadcast.consume();
		let consumer2 = consumer.clone();

		// Subscribe to a dynamic track -- pends until handler responds.
		let consumer_clone = consumer.clone();
		let track1_handle =
			tokio::spawn(async move { consumer_clone.subscribe_track(&Track::new("track1")).await.unwrap() });

		// Let the subscribe_track call execute and queue the request.
		tokio::task::yield_now().await;

		// Get the request -- there should be exactly one.
		let request = dynamic.assert_request();
		dynamic.assert_no_request();
		assert_eq!(request.info.name, "track1");

		// Handler creates the producer, inserts it, and responds.
		let mut track_producer = request.info.clone().produce();
		dynamic.insert_track(&track_producer).unwrap();
		request.respond(track_producer.consume());

		// The subscriber should now have a consumer.
		let mut track1 = track1_handle.await.unwrap();
		track1.assert_no_group();

		// Dedup: subscribing to the same track should return the existing one.
		let mut track1_dup = consumer2.assert_subscribe_track(&Track::new("track1")).await;
		track1_dup.assert_is_clone(&track1);

		// Append a group and make sure both get it.
		track_producer.append_group().unwrap();
		track1.assert_group();
		track1_dup.assert_group();

		// Make sure that pending requests are rejected when dynamic is dropped.
		let consumer3 = consumer.clone();
		let track2_handle = tokio::spawn(async move { consumer3.subscribe_track(&Track::new("track2")).await });
		tokio::task::yield_now().await;
		drop(dynamic);

		let track2_result = track2_handle.await.unwrap();
		assert!(track2_result.is_err(), "should have errored");
	}

	#[tokio::test]
	async fn stale_producer() {
		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();
		let consumer = broadcast.consume();

		// Subscribe to a track.
		let consumer_clone = consumer.clone();
		let track1_handle =
			tokio::spawn(async move { consumer_clone.subscribe_track(&Track::new("track1")).await.unwrap() });
		tokio::task::yield_now().await;

		// Handle the request.
		let request = dynamic.assert_request();
		let mut producer1 = request.info.clone().produce();
		dynamic.insert_track(&producer1).unwrap();
		request.respond(producer1.consume());

		let track1 = track1_handle.await.unwrap();

		// Close the producer (simulating publisher disconnect).
		producer1.append_group().unwrap();
		producer1.finish().unwrap();
		drop(producer1);

		// The consumer should see the track as closed.
		track1.assert_closed();

		// Subscribe again to the same track.
		let consumer_clone = consumer.clone();
		let track2_handle =
			tokio::spawn(async move { consumer_clone.subscribe_track(&Track::new("track1")).await.unwrap() });
		tokio::task::yield_now().await;

		// Should be a new request.
		let request2 = dynamic.assert_request();
		let mut producer2 = request2.info.clone().produce();
		dynamic.insert_track(&producer2).unwrap();
		request2.respond(producer2.consume());

		let mut track2 = track2_handle.await.unwrap();
		track2.assert_not_closed();
		track2.assert_not_clone(&track1);

		producer2.append_group().unwrap();
		track2.assert_group();
	}

	#[tokio::test]
	async fn dedup_pending() {
		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();
		let consumer = broadcast.consume();

		// Two concurrent subscribers for the same track.
		let c1 = consumer.clone();
		let c2 = consumer.clone();
		let h1 = tokio::spawn(async move { c1.subscribe_track(&Track::new("t")).await.unwrap() });
		let h2 = tokio::spawn(async move { c2.subscribe_track(&Track::new("t")).await.unwrap() });
		tokio::task::yield_now().await;

		// Only one request should be queued.
		let request = dynamic.assert_request();
		dynamic.assert_no_request();

		// Respond.
		let producer = request.info.clone().produce();
		dynamic.insert_track(&producer).unwrap();
		request.respond(producer.consume());

		let t1 = h1.await.unwrap();
		let t2 = h2.await.unwrap();
		t1.assert_is_clone(&t2);
	}

	#[tokio::test]
	async fn requested_unused() {
		tokio::time::pause();
		let broadcast = Broadcast::new().produce();
		let mut dynamic = broadcast.dynamic();

		// Subscribe to a track.
		let c1 = broadcast.consume();
		let c1_clone = c1.clone();
		let h1 = tokio::spawn(async move { c1_clone.subscribe_track(&Track::new("unknown_track")).await.unwrap() });
		tokio::task::yield_now().await;

		// Handle the request.
		let request = dynamic.assert_request();
		let producer1 = request.info.clone().produce();
		dynamic.insert_track(&producer1).unwrap();
		request.respond(producer1.consume());

		let consumer1 = h1.await.unwrap();

		// The track producer should NOT be unused yet because there's a consumer.
		assert!(
			producer1.unused().now_or_never().is_none(),
			"track producer should be used"
		);

		// Making a new consumer will keep the producer alive.
		let consumer2 = c1.assert_subscribe_track(&Track::new("unknown_track")).await;
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
		let c2_clone = c2.clone();
		let h2 = tokio::spawn(async move { c2_clone.subscribe_track(&Track::new("unknown_track")).await });
		tokio::task::yield_now().await;

		let request2 = dynamic.assert_request();
		let producer2 = request2.info.clone().produce();
		dynamic.insert_track(&producer2).unwrap();
		request2.respond(producer2.consume());
		let _ = h2.await.unwrap();

		drop(c2);
		assert!(
			producer2.unused().now_or_never().is_some(),
			"track producer should be unused after consumer is dropped"
		);
	}
}
