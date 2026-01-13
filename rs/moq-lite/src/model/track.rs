//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackConsumer] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequest number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [TrackProducer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackConsumer] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Tranport is to not block on them.
//! streams will be cached for a potentially limited duration added to the unreliable nature.
//! A cloned [TrackConsumer] will receive a copy of all new stream going forward (fanout).
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use web_async::FuturesExt;

use super::{Consumer, Group, GroupConsumer, GroupProducer, Producer, ProducerWeak};
use crate::{
	Delivery, DeliveryConsumer, DeliveryProducer, Error, ExpiresConsumer, ExpiresProducer, Produce, Result, Subscriber,
	Subscribers,
};

use std::{
	borrow::Cow,
	collections::VecDeque,
	fmt,
	future::Future,
	ops::{Deref, DerefMut},
	sync::Arc,
};

/// Static information about a track
///
/// Only used to make accessing the name easy/fast.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Track {
	pub name: Arc<String>,
}

impl fmt::Display for Track {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.name)
	}
}

impl Track {
	pub fn new<T: ToString>(name: T) -> Self {
		Self {
			name: Arc::new(name.to_string()),
		}
	}

	pub fn as_str(&self) -> &str {
		&self.name
	}
}

impl From<&str> for Track {
	fn from(name: &str) -> Self {
		Self {
			name: Arc::new(name.to_string()),
		}
	}
}

impl From<String> for Track {
	fn from(name: String) -> Self {
		Self { name: Arc::new(name) }
	}
}

impl From<&String> for Track {
	fn from(name: &String) -> Self {
		Self {
			name: Arc::new(name.clone()),
		}
	}
}

impl From<&Track> for Track {
	fn from(track: &Track) -> Self {
		track.clone()
	}
}

impl From<Cow<'_, str>> for Track {
	fn from(name: Cow<'_, str>) -> Self {
		Self {
			name: Arc::new(name.into_owned()),
		}
	}
}

impl From<Arc<String>> for Track {
	fn from(name: Arc<String>) -> Self {
		Self { name }
	}
}

impl AsRef<str> for Track {
	fn as_ref(&self) -> &str {
		&self.name
	}
}

#[derive(Debug, Default)]
struct State {
	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<Produce<GroupProducer, GroupConsumer>>>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// The highest sequence number received.
	max: Option<u64>,
}

impl State {
	fn create_group(&mut self, info: Group, expires: ExpiresProducer) -> Result<GroupProducer> {
		let group = GroupProducer::new(info.clone(), expires);

		// As a sanity check, make sure this is not a duplicate.
		if self
			.groups
			.iter()
			.filter_map(|g| g.as_ref())
			.any(|g| g.producer.sequence == group.sequence)
		{
			return Err(Error::Duplicate);
		}

		self.max = Some(self.max.unwrap_or_default().max(group.sequence));

		self.groups.push_back(Some(Produce {
			consumer: group.consume(),
			producer: group.clone(),
		}));

		Ok(group)
	}

	fn append_group(&mut self, expires: ExpiresProducer) -> GroupProducer {
		let sequence = match self.max {
			Some(sequence) => sequence + 1,
			None => 0,
		};
		self.max = Some(sequence);

		let group = GroupProducer::new(Group { sequence }, expires);

		self.groups.push_back(Some(Produce {
			consumer: group.consume(),
			producer: group.clone(),
		}));

		group
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone, Debug)]
pub struct TrackProducer {
	info: Track,
	state: Producer<State>,
	subscribers: Subscribers,
	delivery: DeliveryProducer,
	expires: ExpiresProducer,
}

impl TrackProducer {
	pub fn new<T: Into<Track>>(info: T, delivery: Delivery) -> Self {
		let info = info.into();

		let delivery = DeliveryProducer::new(delivery);

		Self {
			state: Producer::default(),
			expires: ExpiresProducer::new(delivery.consume()),
			delivery,
			subscribers: Default::default(),
			info,
		}
	}

	pub fn info(&self) -> Track {
		self.info.clone()
	}

	/// A handle to update the delivery information.
	pub fn delivery(&mut self) -> &mut DeliveryProducer {
		&mut self.delivery
	}

	/// Information about all of the subscribers of this track.
	pub fn subscribers(&mut self) -> &mut Subscribers {
		&mut self.subscribers
	}

	/// Return a handle controlling when groups are expired.
	pub fn expires(&mut self) -> &mut ExpiresProducer {
		&mut self.expires
	}

	/// Create a new [GroupProducer] with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group<T: Into<Group>>(&mut self, info: T) -> Result<GroupProducer> {
		self.state
			.modify(|state| state.create_group(info.into(), self.expires.clone()))?
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		self.state.modify(|state| state.append_group(self.expires.clone()))
	}

	pub fn close(&mut self) -> Result<()> {
		self.state.close()
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		self.state.abort(err)
	}

	/// Create a new consumer for the track.
	pub fn subscribe(&self, delivery: Delivery) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			subscriber: self.subscribers.subscribe(delivery),
			index: 0,
			expires: self.expires.consume(),
			delivery: self.delivery.consume(),
		}
	}

	/// Block until there are no active consumers.
	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let state = self.state.clone();
		async move { state.unused().await }
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	pub(super) fn weak(&self) -> TrackProducerWeak {
		TrackProducerWeak {
			info: self.info.clone(),
			state: self.state.weak(),
			subscribers: self.subscribers.clone(),
			delivery: self.delivery.clone(),
			expires: self.expires.clone(),
		}
	}
}

impl Deref for TrackProducer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

#[derive(Clone)]
pub(super) struct TrackProducerWeak {
	info: Track,
	state: ProducerWeak<State>,
	subscribers: Subscribers,
	delivery: DeliveryProducer,
	expires: ExpiresProducer,
}

impl TrackProducerWeak {
	pub fn upgrade(self) -> Result<TrackProducer> {
		Ok(TrackProducer {
			info: self.info,
			state: self.state.upgrade()?,
			subscribers: self.subscribers,
			delivery: self.delivery,
			expires: self.expires,
		})
	}
}

/// A consumer for a track, used to read groups.
///
/// If the consumer is cloned, it will receive a copy of all unread groups.
#[derive(Debug)]
pub struct TrackConsumer {
	info: Track,

	state: Consumer<State>,

	subscriber: Subscriber,

	expires: ExpiresConsumer,

	// We last returned this group, factoring in offset
	index: usize,

	delivery: DeliveryConsumer,
}

impl TrackConsumer {
	pub fn info(&self) -> Track {
		self.info.clone()
	}

	/// Return the next group received over the network, in any order.
	///
	/// See [TrackConsumerOrdered] if you're willing to buffer groups in order.
	///
	/// NOTE: This can have gaps due to congestion.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		loop {
			// Wait until there's a new latest group or the track is closed.
			let state = self
				.state
				.wait_for(|state| self.index < state.offset + state.groups.len())
				.await?;

			let range = self.index.saturating_sub(state.offset)..state.groups.len();
			if range.is_empty() {
				// Closed
				return Ok(None);
			}

			for i in range {
				self.index = state.offset + i + 1;

				// If None, the group has expired out of order.
				if let Some(group) = &state.groups[i] {
					// NOTE: This group might be expired from the consumer's perspective.
					// Return than skip it, we return expires groups because it's still useful information.
					return Ok(Some(group.consumer.clone()));
				}
			}
		}
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		self.state.closed().await
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	/// Return a handle allowing you to update the subscriber's priority/max_latency.
	pub fn subscriber(&mut self) -> &mut Subscriber {
		&mut self.subscriber
	}

	/// Return a handle to detect when groups are expired.
	///
	/// This is used internally, but worth exporting I guess.
	pub fn expires(&mut self) -> &mut ExpiresConsumer {
		&mut self.expires
	}

	/// Return a handle to update the delivery information.
	pub fn delivery(&mut self) -> &mut DeliveryConsumer {
		&mut self.delivery
	}

	/// Convert to a helper that returns groups in order, if possible.
	pub fn ordered(self) -> TrackConsumerOrdered {
		TrackConsumerOrdered::new(self)
	}
}

impl Deref for TrackConsumer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl TrackConsumer {
	pub fn assert_group(&mut self) -> GroupConsumer {
		self.next_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_no_group(&mut self) {
		assert!(
			self.next_group().now_or_never().is_none(),
			"next group would not have blocked"
		);
	}

	pub fn assert_not_closed(&self) {
		assert!(self.closed().now_or_never().is_none(), "should not be closed");
	}

	pub fn assert_closed(&self) {
		assert!(self.closed().now_or_never().is_some(), "should be closed");
	}

	// TODO assert specific errors after implementing PartialEq
	pub fn assert_error(&self) {
		assert!(
			self.closed().now_or_never().expect("should not block").is_err(),
			"should be error"
		);
	}

	pub fn assert_is_clone(&self, other: &Self) {
		assert!(self.is_clone(other), "should be clone");
	}

	pub fn assert_not_clone(&self, other: &Self) {
		assert!(!self.is_clone(other), "should not be clone");
	}
}

/// A [TrackConsumer] that returns groups in creation order, if possible.
///
/// It's recommended to set [Delivery::ordered] too if you REALLY want head-of-line blocking.
/// The user experience would be to buffer rather than skip any groups, except in severe congestion.
///
/// With [Delivery::ordered] not set, we will try our best to return groups in order up to `max_latency`.
/// This produces a hybrid experience where we'll buffer up until a point then skip ahead to newer groups.
//
// TODO: There's no group dropped message (yet), so we guess based on min/max timestamps.
pub struct TrackConsumerOrdered {
	track: TrackConsumer,
	expected: u64,
	pending: VecDeque<GroupConsumer>,
}

impl TrackConsumerOrdered {
	pub fn new(track: TrackConsumer) -> Self {
		Self {
			track,
			expected: 0,
			pending: VecDeque::new(),
		}
	}

	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		let mut expires = self.track.expires().clone();

		loop {
			tokio::select! {
				biased;
				// Get the next group from the track.
				Some(group) = self.track.next_group().transpose() => {
					let group = group?;

					// If we're looking for this sequence number, return it.
					if group.sequence == self.expected {
						self.expected += 1;
						return Ok(Some(group));
					}

					// If it's old, skip it.
					if group.sequence < self.expected {
						continue;
					}

					// If it's new, insert it into the buffered queue based on the sequence number ascending.
					let index = self.pending.partition_point(|g| g.sequence < group.sequence);
					self.pending.insert(index, group);
				}
				Some(next) = async {
					loop {
						// Get the oldest group in the buffered queue.
						let first = self.pending.front()?;

						// If the minimum sequence is not what we're looking for, wait until it would be expired.
						if first.sequence != self.expected {
							// Wait until the first frame of the group has been received.
							let Ok(Some(frame)) = first.clone().next_frame().await else {
								// The group has no frames, just skip it.
								self.pending.pop_front();
								continue;
							};

							// Wait until the first frame of the group would have been expired.
							// This doesn't mean the entire group is expired, because that uses the max_timestamp.
							// But even if the group has one frame this will still unstuck the consumer.
							expires.wait_expired(first.sequence, frame.instant).await;
						}

						// Return the minimum group and skip over any gap.
						let first = self.pending.pop_front().unwrap();
						self.expected = first.sequence + 1;

						return Some(first);
					}
				} => {
					// We found the next group in order, so update the expected sequence number.
					self.expected = next.sequence + 1;
					return Ok(Some(next));
				}
			}
		}
	}
}

impl Deref for TrackConsumerOrdered {
	type Target = TrackConsumer;

	fn deref(&self) -> &Self::Target {
		&self.track
	}
}

impl DerefMut for TrackConsumerOrdered {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.track
	}
}

#[cfg(test)]
impl TrackConsumerOrdered {
	pub fn assert_next_group(&mut self) -> GroupConsumer {
		use futures::FutureExt;
		self.next_group()
			.now_or_never()
			.expect("next_group blocked")
			.expect("next_group error")
			.expect("next_group returned None")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Time;
	use bytes::Bytes;

	#[test]
	fn test_track_new() {
		let track = Track::new("test");
		assert_eq!(track.as_str(), "test");
	}

	#[test]
	fn test_track_from_str() {
		let track: Track = "test".into();
		assert_eq!(track.as_str(), "test");
	}

	#[test]
	fn test_track_from_string() {
		let track: Track = String::from("test").into();
		assert_eq!(track.as_str(), "test");
	}

	#[tokio::test]
	async fn test_track_append_group() {
		let mut producer = TrackProducer::new("test", Delivery::default());
		let mut consumer = producer.subscribe(Delivery::default());

		// Append first group
		let mut group1 = producer.append_group().unwrap();
		assert_eq!(group1.sequence, 0);

		// Write a frame to the group
		let instant = Time::from_millis(100).unwrap();
		group1.write_frame(Bytes::from("data1"), instant).unwrap();
		group1.close().unwrap();

		// Consumer should receive the group
		let mut group1_consumer = consumer.assert_group();
		assert_eq!(group1_consumer.sequence, 0);
		let data = group1_consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("data1"));

		// Append second group
		let mut group2 = producer.append_group().unwrap();
		assert_eq!(group2.sequence, 1);
		group2.write_frame(Bytes::from("data2"), instant).unwrap();
		group2.close().unwrap();

		let mut group2_consumer = consumer.assert_group();
		assert_eq!(group2_consumer.sequence, 1);
		let data = group2_consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("data2"));
	}

	#[tokio::test]
	async fn test_track_create_group() {
		let mut producer = TrackProducer::new("test", Delivery::default());
		let mut consumer = producer.subscribe(Delivery::default());

		// Create a group with specific sequence
		let mut group = producer.create_group(Group { sequence: 42 }).unwrap();
		assert_eq!(group.sequence, 42);

		let instant = Time::from_millis(100).unwrap();
		group.write_frame(Bytes::from("hello"), instant).unwrap();
		group.close().unwrap();

		let group_consumer = consumer.assert_group();
		assert_eq!(group_consumer.sequence, 42);
	}

	#[tokio::test]
	async fn test_track_duplicate_group() {
		let mut producer = TrackProducer::new("test", Delivery::default());

		// Create first group with sequence 5
		let _group1 = producer.create_group(Group { sequence: 5 }).unwrap();

		// Try to create another group with the same sequence
		let result = producer.create_group(Group { sequence: 5 });
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn test_track_multiple_consumers() {
		let mut producer = TrackProducer::new("test", Delivery::default());
		let mut consumer1 = producer.subscribe(Delivery::default());
		let mut consumer2 = producer.subscribe(Delivery::default());

		let mut group = producer.append_group().unwrap();
		let instant = Time::from_millis(100).unwrap();
		group.write_frame(Bytes::from("shared"), instant).unwrap();
		group.close().unwrap();

		// Both consumers should receive the group
		let mut g1 = consumer1.assert_group();
		let mut g2 = consumer2.assert_group();

		assert_eq!(g1.read_frame().await.unwrap().unwrap(), Bytes::from("shared"));
		assert_eq!(g2.read_frame().await.unwrap().unwrap(), Bytes::from("shared"));
	}

	#[tokio::test]
	async fn test_track_close() {
		let mut producer = TrackProducer::new("test", Delivery::default());
		let consumer = producer.subscribe(Delivery::default());

		producer.close().unwrap();

		// Consumer should detect the track is closed
		consumer.assert_closed();
	}

	#[tokio::test]
	async fn test_track_abort() {
		let mut producer = TrackProducer::new("test", Delivery::default());
		let consumer = producer.subscribe(Delivery::default());

		producer.abort(Error::Cancel).unwrap();

		// Consumer should detect the error
		consumer.assert_error();
	}

	#[tokio::test]
	async fn test_track_info() {
		let track_info = Track::new("my_track");
		let producer = TrackProducer::new(track_info.clone(), Delivery::default());
		let consumer = producer.subscribe(Delivery::default());

		assert_eq!(producer.info().as_str(), "my_track");
		assert_eq!(consumer.info().as_str(), "my_track");
	}

	#[tokio::test]
	async fn test_track_is_clone() {
		let producer1 = TrackProducer::new("test", Delivery::default());
		let producer2 = producer1.clone();
		let producer3 = TrackProducer::new("test", Delivery::default());

		assert!(producer1.is_clone(&producer2));
		assert!(!producer1.is_clone(&producer3));

		let consumer1 = producer1.subscribe(Delivery::default());
		let consumer2 = producer1.subscribe(Delivery::default());
		let consumer3 = producer3.subscribe(Delivery::default());

		consumer1.assert_is_clone(&consumer2);
		consumer1.assert_not_clone(&consumer3);
	}

	#[tokio::test]
	async fn test_track_delivery_updates() {
		let mut producer = TrackProducer::new("test", Delivery::default());
		let mut consumer = producer.subscribe(Delivery::default());

		// Update delivery info
		let new_delivery = Delivery {
			priority: 10,
			max_latency: Time::from_millis(500).unwrap(),
			ordered: true,
		};
		producer.delivery().update(new_delivery);

		// Consumer should see the update
		let updated = consumer.delivery().changed().await.unwrap();
		assert_eq!(updated.priority, 10);
		assert_eq!(updated.max_latency, Time::from_millis(500).unwrap());
		assert!(updated.ordered);
	}

	#[tokio::test]
	async fn test_track_ordered_consumer() {
		// Set max_latency to allow buffering out-of-order groups
		let delivery = Delivery {
			max_latency: Time::from_millis(500).unwrap(),
			..Default::default()
		};
		let mut producer = TrackProducer::new("test", delivery);
		let consumer = producer.subscribe(delivery);
		let mut ordered = consumer.ordered();

		// Create groups out of order
		let mut group2 = producer.create_group(Group { sequence: 1 }).unwrap();
		let mut group3 = producer.create_group(Group { sequence: 2 }).unwrap();
		let mut group1 = producer.create_group(Group { sequence: 0 }).unwrap();

		// Use timestamps that correspond to when each group was created
		// Group 0 has timestamp 0ms, group 1 has 100ms, group 2 has 200ms
		let t0 = Time::from_millis(0).unwrap();
		let t1 = Time::from_millis(100).unwrap();
		let t2 = Time::from_millis(200).unwrap();

		// Write and close in reverse order (groups arrive out of order)
		group3.write_frame(Bytes::from("g3"), t2).unwrap();
		group3.close().unwrap();

		group1.write_frame(Bytes::from("g1"), t0).unwrap();
		group1.close().unwrap();

		group2.write_frame(Bytes::from("g2"), t1).unwrap();
		group2.close().unwrap();

		// Ordered consumer should return them in order despite arriving out of order
		let g = ordered.assert_next_group();
		assert_eq!(g.sequence, 0);

		let g = ordered.assert_next_group();
		assert_eq!(g.sequence, 1);

		let g = ordered.assert_next_group();
		assert_eq!(g.sequence, 2);
	}

	#[tokio::test]
	async fn test_track_ordered_consumer_skip_expired() {
		// Set max_latency to allow some buffering but not infinite
		let delivery = Delivery {
			max_latency: Time::from_millis(150).unwrap(),
			..Default::default()
		};
		let mut producer = TrackProducer::new("test", delivery);
		let consumer = producer.subscribe(delivery);
		let mut ordered = consumer.ordered();

		// Create groups where we'll skip group 1
		let mut group0 = producer.create_group(Group { sequence: 0 }).unwrap();
		let mut group2 = producer.create_group(Group { sequence: 2 }).unwrap();
		let mut group3 = producer.create_group(Group { sequence: 3 }).unwrap();

		// Timestamps need to satisfy: group2_instant + max_latency <= max_instant
		// So: 100ms + 150ms <= 300ms ✓ (250 <= 300)
		let t0 = Time::from_millis(0).unwrap();
		let t2 = Time::from_millis(100).unwrap();
		let t3 = Time::from_millis(300).unwrap();

		// Write groups 0, 2, 3 (skip group 1)
		group0.write_frame(Bytes::from("g0"), t0).unwrap();
		group0.close().unwrap();

		group2.write_frame(Bytes::from("g2"), t2).unwrap();
		group2.close().unwrap();

		group3.write_frame(Bytes::from("g3"), t3).unwrap();
		group3.close().unwrap();

		// Should get group 0 immediately
		let g = ordered.assert_next_group();
		assert_eq!(g.sequence, 0);

		// Should skip group 1 (missing) and get group 2
		// The ordered consumer waits for the first buffered group (group 2) to expire:
		// - group 2 sequence (2) < max_group (3) ✓
		// - group 2 instant (100ms) + max_latency (150ms) = 250ms <= max_instant (300ms) ✓
		// So group 2 should be expired and returned immediately, skipping group 1
		let g = ordered.assert_next_group();
		assert_eq!(g.sequence, 2);

		// Then get group 3
		let g = ordered.assert_next_group();
		assert_eq!(g.sequence, 3);
	}

	#[tokio::test]
	async fn test_track_unused() {
		let producer = TrackProducer::new("test", Delivery::default());

		// Create and drop a consumer
		let consumer = producer.subscribe(Delivery::default());
		drop(consumer);

		// Producer should eventually become unused
		producer.unused().await;
	}
}
