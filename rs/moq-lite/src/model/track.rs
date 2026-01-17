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

use super::{Consumer, Group, GroupConsumer, GroupProducer, Producer};
use crate::{
	model::waiter::{waiter_fn, Waiter},
	Delivery, Error, Time,
};

use std::{
	borrow::Cow,
	collections::{HashSet, VecDeque},
	fmt,
	ops::Deref,
	sync::Arc,
	task::Poll,
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

/// Delivery information for a track.
///
/// Both the publisher and subscriber can set their own values.
#[derive(Clone, Debug, PartialEq, Eq, Copy, Default)]
pub struct TrackDelivery {
	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: Time,

	/// Try to deliver groups in sequence order (for VOD).
	pub ordered: bool,
}

#[derive(Debug, Default)]
struct State {
	// Groups in order of arrival.
	// If None, the group has expired but was not in the front of the queue.
	groups: VecDeque<Option<GroupProducer>>,

	// Groups in sequence order.
	ordered: VecDeque<GroupProducer>,

	// Sequences that have been seen, for sanity checking.
	// NOTE: This is not exhaustive, as gaps are valid and we don't care enough to track them.
	duplicates: HashSet<u64>,

	// +1 every time we remove a group from the front.
	offset: usize,

	// The highest sequence number received.
	max_sequence: Option<u64>,

	// No more groups will be created.
	fin: bool,

	// The producer's delivery settings for this track.
	delivery: Delivery,
}

impl State {
	fn append_group(&mut self) -> Result<GroupProducer, Error> {
		let sequence = self.max_sequence.map(|max| max + 1).unwrap_or(0);
		self.create_group(sequence)
	}

	fn create_group<T: Into<Group>>(&mut self, group: T) -> Result<GroupProducer, Error> {
		let group = group.into();

		if self.fin {
			return Err(Error::Closed);
		}

		// As a sanity check, make sure this is not a duplicate.
		if !self.duplicates.insert(group.sequence) {
			return Err(Error::Duplicate);
		}

		let group = GroupProducer::new(group);
		self.max_sequence = Some(self.max_sequence.unwrap_or_default().max(group.sequence));

		// Store groups in arrival order.
		self.groups.push_back(Some(group.clone()));

		// Store groups in sequence order.
		let index = self
			.ordered
			.binary_search_by_key(&group.sequence, |group| group.sequence)
			.unwrap_or_else(|i| i);
		self.ordered.insert(index, group.clone());

		Ok(group)
	}

	fn poll_any_group(&self, index: &mut usize, expected: &mut u64) -> Poll<Option<GroupConsumer>> {
		let i = index.saturating_sub(self.offset);

		while let Some(group) = self.groups.get(i) {
			// Skip over these groups next time; we've already checked/returned them.
			*index = i + self.offset;

			if let Some(group) = group {
				if group.sequence >= *expected {
					*expected = group.sequence + 1;
					return Poll::Ready(Some(group.consume()));
				}
			}
		}

		if self.fin {
			return Poll::Ready(None);
		}

		Poll::Pending
	}

	fn poll_next_group(&self, waiter: &Waiter<'_>, expected: &mut u64) -> Poll<Option<GroupConsumer>> {
		// TODO we should search backwards, because most of the time index will at, or near the end.
		let index = match self.ordered.binary_search_by_key(expected, |group| group.sequence) {
			Ok(index) => {
				// We found the group we want to return next, so do it.
				// NOTE: We don't even care if it has a timestamp or not.
				*expected += 1;
				return Poll::Ready(Some(self.ordered.get(index).unwrap().consume()));
			}
			Err(index) => index,
		};

		// Loop over forwards to find the first group with a timestamp, using the minimum.
		let min = self.ordered.iter().skip(index).find_map(|group| {
			if let Poll::Ready(Ok(timestamp)) = group.consume().poll_timestamp(waiter) {
				Some((group, timestamp.0))
			} else {
				None
			}
		});

		// Loop over backwards to find the last group with a timestamp, using the maximum.
		let max = self.ordered.iter().skip(index).rev().find_map(|group| {
			if let Poll::Ready(Ok(timestamp)) = group.consume().poll_timestamp(waiter) {
				Some((group, timestamp.1))
			} else {
				None
			}
		});

		// Okay if there's a minimum and maximum, check if enough time has passed.
		if let (Some(min), Some(max)) = (min, max) {
			if min.1 + max.1 >= self.delivery.max_latency {
				// If so, return the minimum group, skipping over anything before it.
				*expected = min.0.sequence + 1;
				return Poll::Ready(Some(min.0.consume()));
			}
		}

		Poll::Pending
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone, Debug)]
pub struct TrackProducer {
	info: Track,
	state: Producer<State>,
}

impl TrackProducer {
	pub fn new<T: Into<Track>>(info: T) -> Self {
		let info = info.into();
		Self {
			state: Producer::default(),
			info,
		}
	}

	pub fn info(&self) -> Track {
		self.info.clone()
	}

	/// Create a new [GroupProducer] with the given info.
	///
	/// Returns an error if the track is closed.
	pub fn create_group<T: Into<Group>>(&mut self, info: T) -> Result<GroupProducer, Error> {
		self.state.modify()?.create_group(info)
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer, Error> {
		self.state.modify()?.append_group()
	}

	pub fn close(&mut self) -> Result<(), Error> {
		self.state.modify()?.fin = true;
		Ok(())
	}

	pub fn abort(self, err: Error) -> Result<(), Error> {
		self.state.close(err)
	}

	/// Update the delivery settings for this track.
	pub fn update_delivery(&mut self, delivery: Delivery) -> Result<(), Error> {
		self.state.modify()?.delivery = delivery;
		Ok(())
	}

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
			expected: 0,
		}
	}

	/// Block until there are no active consumers.
	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub async fn unused(&self) -> Result<(), Error> {
		self.state.unused().await
	}
}

impl Deref for TrackProducer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// A consumer for a track, used to read groups.
///
/// If the consumer is cloned, it will receive a copy of all unread groups.
#[derive(Debug)]
pub struct TrackConsumer {
	info: Track,

	state: Consumer<State>,

	// We want to return this group next for `any_group`, factoring in offset
	index: usize,

	// The sequence number we expect next for `next_group`.
	expected: u64,
}

impl TrackConsumer {
	pub fn info(&self) -> Track {
		self.info.clone()
	}

	/// Return the next group received over the network, in any order.
	///
	/// NOTE: There can be gaps due to congestion.
	pub async fn any_group(&mut self) -> Result<Option<GroupConsumer>, Error> {
		waiter_fn(move |waiter| self.poll_any_group(waiter)).await
	}

	pub fn poll_any_group(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Option<GroupConsumer>, Error>> {
		self.state.poll(waiter, |state| {
			state.poll_any_group(&mut self.index, &mut self.expected)
		})
	}

	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>, Error> {
		waiter_fn(move |waiter| self.poll_next_group(waiter)).await
	}

	pub fn poll_next_group(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Option<GroupConsumer>, Error>> {
		self.state
			.poll(waiter, |state| state.poll_next_group(waiter, &mut self.expected))
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Error {
		self.state.closed().await
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
	pub fn assert_any_group(&mut self) -> GroupConsumer {
		self.any_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_next_group(&mut self) -> GroupConsumer {
		self.next_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_no_group(&mut self) {
		assert!(
			self.any_group().now_or_never().is_none(),
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
		self.closed().now_or_never().expect("should not block");
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
		let mut producer = TrackProducer::new("test");
		let mut consumer = producer.consume();

		// Append first group
		let mut group1 = producer.append_group().unwrap();
		assert_eq!(group1.sequence, 0);

		// Write a frame to the group
		let instant = Time::from_millis(100).unwrap();
		group1.write_frame(Bytes::from("data1"), instant).unwrap();
		group1.final_frame().unwrap();

		// Consumer should receive the group
		let mut group1_consumer = consumer.assert_any_group();
		assert_eq!(group1_consumer.sequence, 0);
		let data = group1_consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("data1"));

		// Append second group
		let mut group2 = producer.append_group().unwrap();
		assert_eq!(group2.sequence, 1);
		group2.write_frame(Bytes::from("data2"), instant).unwrap();
		group2.final_frame().unwrap();

		let mut group2_consumer = consumer.assert_any_group();
		assert_eq!(group2_consumer.sequence, 1);
		let data = group2_consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("data2"));
	}

	#[tokio::test]
	async fn test_track_create_group() {
		let mut producer = TrackProducer::new("test");
		let mut consumer = producer.consume();

		// Create a group with specific sequence
		let mut group = producer.create_group(Group { sequence: 42 }).unwrap();
		assert_eq!(group.sequence, 42);

		let instant = Time::from_millis(100).unwrap();
		group.write_frame(Bytes::from("hello"), instant).unwrap();
		group.final_frame().unwrap();

		let group_consumer = consumer.assert_any_group();
		assert_eq!(group_consumer.sequence, 42);
	}

	#[tokio::test]
	async fn test_track_duplicate_group() {
		let mut producer = TrackProducer::new("test");

		// Create first group with sequence 5
		let _group1 = producer.create_group(Group { sequence: 5 }).unwrap();

		// Try to create another group with the same sequence
		let result = producer.create_group(Group { sequence: 5 });
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn test_track_multiple_consumers() {
		let mut producer = TrackProducer::new("test");
		let mut consumer1 = producer.consume();
		let mut consumer2 = producer.consume();

		let mut group = producer.append_group().unwrap();
		let instant = Time::from_millis(100).unwrap();
		group.write_frame(Bytes::from("shared"), instant).unwrap();
		group.final_frame().unwrap();

		// Both consumers should receive the group
		let mut g1 = consumer1.assert_any_group();
		let mut g2 = consumer2.assert_any_group();

		assert_eq!(g1.read_frame().await.unwrap().unwrap(), Bytes::from("shared"));
		assert_eq!(g2.read_frame().await.unwrap().unwrap(), Bytes::from("shared"));
	}

	#[tokio::test]
	async fn test_track_close() {
		let mut producer = TrackProducer::new("test");
		let consumer = producer.consume();

		producer.close().unwrap();

		// Consumer should detect the track is closed
		consumer.assert_closed();
	}

	#[tokio::test]
	async fn test_track_abort() {
		let producer = TrackProducer::new("test");
		let consumer = producer.consume();

		producer.abort(Error::Cancel).unwrap();

		// Consumer should detect the error
		consumer.assert_error();
	}

	#[tokio::test]
	async fn test_track_info() {
		let track_info = Track::new("my_track");
		let producer = TrackProducer::new(track_info.clone());
		let consumer = producer.consume();

		assert_eq!(producer.info().as_str(), "my_track");
		assert_eq!(consumer.info().as_str(), "my_track");
	}

	#[tokio::test]
	async fn test_track_ordered_consumer() {
		// Set max_latency to allow buffering out-of-order groups
		let delivery = Delivery {
			max_latency: Time::from_millis(500).unwrap(),
			..Default::default()
		};
		let mut producer = TrackProducer::new("test");
		let mut consumer = producer.consume();

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
		group3.final_frame().unwrap();

		group1.write_frame(Bytes::from("g1"), t0).unwrap();
		group1.final_frame().unwrap();

		group2.write_frame(Bytes::from("g2"), t1).unwrap();
		group2.final_frame().unwrap();

		// Ordered consumer should return them in order despite arriving out of order
		let g = consumer.assert_next_group();
		assert_eq!(g.sequence, 0);

		let g = consumer.assert_next_group();
		assert_eq!(g.sequence, 1);

		let g = consumer.assert_next_group();
		assert_eq!(g.sequence, 2);
	}

	#[tokio::test]
	async fn test_track_ordered_consumer_skip_expired() {
		// Set max_latency to allow some buffering but not infinite
		let delivery = Delivery {
			max_latency: Time::from_millis(150).unwrap(),
			..Default::default()
		};
		let mut producer = TrackProducer::new("test");
		let mut consumer = producer.consume();

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
		group0.final_frame().unwrap();

		group2.write_frame(Bytes::from("g2"), t2).unwrap();
		group2.final_frame().unwrap();

		group3.write_frame(Bytes::from("g3"), t3).unwrap();
		group3.final_frame().unwrap();

		// Should get group 0 immediately
		let g = consumer.assert_next_group();
		assert_eq!(g.sequence, 0);

		// Should skip group 1 (missing) and get group 2
		// The ordered consumer waits for the first buffered group (group 2) to expire:
		// - group 2 sequence (2) < max_group (3) ✓
		// - group 2 instant (100ms) + max_latency (150ms) = 250ms <= max_instant (300ms) ✓
		// So group 2 should be expired and returned immediately, skipping group 1
		let g = consumer.assert_next_group();
		assert_eq!(g.sequence, 2);

		// Then get group 3
		let g = consumer.assert_next_group();
		assert_eq!(g.sequence, 3);
	}

	#[tokio::test]
	async fn test_track_unused() {
		let producer = TrackProducer::new("test");

		// Create and drop a consumer
		let consumer = producer.consume();
		drop(consumer);

		// Producer should eventually become unused
		producer.unused().await;
	}
}
