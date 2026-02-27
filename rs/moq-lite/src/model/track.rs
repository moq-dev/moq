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

use crate::{Error, Result};

use super::state::{Consumer, Producer, Weak};
use super::waiter::waiter_fn;
use super::{Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	task::Poll,
	time::Duration,
};

/// Groups older than this are evicted from the track cache (unless they are the max_sequence group).
// TODO: Replace with a configurable cache size.
const MAX_GROUP_AGE: Duration = Duration::from_secs(30);

/// A track is a collection of groups, delivered out-of-order until expired.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	pub name: String,
	pub priority: u8,
}

impl Track {
	pub fn new<T: Into<String>>(name: T) -> Self {
		Self {
			name: name.into(),
			priority: 0,
		}
	}

	pub fn produce(self) -> TrackProducer {
		TrackProducer::new(self)
	}
}

#[derive(Default)]
struct State {
	/// Groups in arrival order. `None` entries are tombstones for evicted groups.
	groups: VecDeque<Option<(GroupProducer, tokio::time::Instant)>>,
	duplicates: HashSet<u64>,
	offset: usize,
	max_sequence: Option<u64>,
	fin: bool,
}

impl State {
	/// Find the next non-tombstoned group at or after `index`.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	fn poll_next_group(&self, index: usize) -> Poll<Option<(GroupProducer, usize)>> {
		let start = index.saturating_sub(self.offset);
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			if let Some((group, _)) = slot {
				return Poll::Ready(Some((group.clone(), self.offset + i)));
			}
		}

		if self.fin { Poll::Ready(None) } else { Poll::Pending }
	}

	fn poll_get_group(&self, sequence: u64) -> Poll<Option<GroupProducer>> {
		// Search for the group with the matching sequence, skipping tombstones.
		// NOTE: Returns Ready(None) if the group was evicted (tombstoned) since
		// it won't be found and max_sequence >= sequence.
		for (group, _) in self.groups.iter().flatten() {
			if group.info.sequence == sequence {
				return Poll::Ready(Some(group.clone()));
			}
		}

		// If we've already seen a newer sequence, the group is gone (or was evicted).
		if let Some(max) = self.max_sequence
			&& max >= sequence
		{
			return Poll::Ready(None);
		}

		if self.fin {
			return Poll::Ready(None);
		}

		Poll::Pending
	}

	/// Evict groups older than MAX_GROUP_AGE, never evicting the max_sequence group.
	///
	/// Groups are in arrival order, so we can stop early when we hit a non-expired,
	/// non-max_sequence group (everything after it arrived even later).
	/// When max_sequence is at the front, we skip past it and tombstone expired groups
	/// behind it.
	fn evict_expired(&mut self, now: tokio::time::Instant) {
		for slot in self.groups.iter_mut() {
			let Some((group, created_at)) = slot else { continue };

			if Some(group.info.sequence) == self.max_sequence {
				continue;
			}

			if now.duration_since(*created_at) <= MAX_GROUP_AGE {
				break;
			}

			self.duplicates.remove(&group.info.sequence);
			*slot = None;
		}

		// Trim leading tombstones to advance the offset.
		while let Some(None) = self.groups.front() {
			self.groups.pop_front();
			self.offset += 1;
		}
	}
}

/// A producer for a track, used to create new groups.
pub struct TrackProducer {
	pub info: Track,
	state: Producer<State>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		Self {
			info,
			state: Producer::default(),
		}
	}

	/// Create a new group with the given sequence number.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = info.produce();

		let mut state = self.state.modify()?;
		if state.fin && group.info.sequence >= state.max_sequence.unwrap_or(0) {
			return Err(Error::Closed);
		}

		if !state.duplicates.insert(group.info.sequence) {
			return Err(Error::Duplicate);
		}

		let now = tokio::time::Instant::now();
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.info.sequence));
		state.groups.push_back(Some((group.clone(), now)));
		state.evict_expired(now);

		Ok(group)
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<GroupProducer> {
		let mut state = self.state.modify()?;
		if state.fin {
			return Err(Error::Closed);
		}

		let now = tokio::time::Instant::now();
		let sequence = state.max_sequence.map_or(0, |s| s + 1);
		let group = Group { sequence }.produce();

		state.duplicates.insert(sequence);
		state.max_sequence = Some(sequence);
		state.groups.push_back(Some((group.clone(), now)));
		state.evict_expired(now);

		Ok(group)
	}

	/// Create a group with a single frame.
	pub fn write_frame<B: Into<bytes::Bytes>>(&mut self, frame: B) -> Result<()> {
		let mut group = self.append_group()?;
		group.write_frame(frame.into())?;
		group.finish()?;
		Ok(())
	}

	/// Mark the last group of the track.
	///
	/// NOTE: The track is not closed yet; old groups can still arrive.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.state.modify()?;
		state.fin = true;
		Ok(())
	}

	/// Abort the track with the given error.
	pub fn close(&mut self, err: Error) -> Result<()> {
		let mut state = self.state.modify()?;

		// Abort all groups still in progress.
		for (group, _) in state.groups.iter_mut().flatten() {
			// Ignore errors, we don't care if the group was already closed.
			group.close(err.clone()).ok();
		}

		state.close(err);
		Ok(())
	}

	/// Create a new consumer for the track, starting at the latest group.
	pub fn consume(&self) -> TrackConsumer {
		let state = self.state.borrow();
		let index = state.offset + state.groups.len().saturating_sub(1);

		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index,
		}
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await
	}

	/// Return true if the track has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.borrow().is_closed()
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			info: self.info.clone(),
			state: self.state.weak(),
		}
	}
}

impl Clone for TrackProducer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			state: self.state.clone(),
		}
	}
}

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

/// A weak reference to a track that doesn't prevent auto-close.
#[derive(Clone)]
pub(crate) struct TrackWeak {
	pub info: Track,
	state: Weak<State>,
}

impl TrackWeak {
	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	pub fn consume(&self) -> TrackConsumer {
		let state = self.state.borrow();
		let index = state.offset + state.groups.len().saturating_sub(1);

		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index,
		}
	}

	pub async fn unused(&self) -> crate::Result<()> {
		self.state.unused().await
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
	}
}

/// A consumer for a track, used to read groups.
#[derive(Clone)]
pub struct TrackConsumer {
	pub info: Track,
	state: Consumer<State>,
	index: usize,
}

impl TrackConsumer {
	/// Return the next group in order.
	///
	/// NOTE: This can have gaps if the reader is too slow or there were network slowdowns.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		let index = self.index;
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_next_group(index))).await?;
		let consumer = res.map(|(producer, found_index)| {
			self.index = found_index + 1;
			producer.consume()
		});
		Ok(consumer)
	}

	/// Block until the group with the given sequence is available.
	///
	/// Returns None if the group is not in the cache and a newer group exists.
	pub async fn get_group(&self, sequence: u64) -> Result<Option<GroupConsumer>> {
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_get_group(sequence))).await?;
		Ok(res.map(|producer| producer.consume()))
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		let err = self.state.closed().await;
		match err {
			Error::Closed | Error::Dropped => Ok(()),
			err => Err(err),
		}
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.is_clone(&other.state)
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

#[cfg(test)]
mod test {
	use super::*;

	/// Helper: count non-tombstoned groups in state.
	fn live_groups(state: &State) -> usize {
		state.groups.iter().flatten().count()
	}

	/// Helper: get the sequence number of the first live group.
	fn first_live_sequence(state: &State) -> u64 {
		state.groups.iter().flatten().next().unwrap().0.info.sequence
	}

	#[tokio::test]
	async fn evict_expired_groups() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();

		// Create 3 groups at time 0.
		producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1
		producer.append_group().unwrap(); // seq 2

		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 3);
			assert_eq!(state.offset, 0);
		}

		// Advance time past the eviction threshold.
		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		// Append a new group to trigger eviction.
		producer.append_group().unwrap(); // seq 3

		// Groups 0, 1, 2 are expired but seq 3 (max_sequence) is kept.
		// Leading tombstones are trimmed, so only seq 3 remains.
		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 3);
			assert_eq!(state.offset, 3);
			assert!(!state.duplicates.contains(&0));
			assert!(!state.duplicates.contains(&1));
			assert!(!state.duplicates.contains(&2));
			assert!(state.duplicates.contains(&3));
		}
	}

	#[tokio::test]
	async fn evict_keeps_max_sequence() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap(); // seq 0

		// Advance time past threshold.
		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		// Append another group; seq 0 is expired and evicted.
		producer.append_group().unwrap(); // seq 1

		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 1);
			assert_eq!(state.offset, 1);
		}
	}

	#[tokio::test]
	async fn no_eviction_when_fresh() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1
		producer.append_group().unwrap(); // seq 2

		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 3);
			assert_eq!(state.offset, 0);
		}
	}

	#[tokio::test]
	async fn consumer_skips_evicted_groups() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap(); // seq 0

		let mut consumer = producer.consume();

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;
		producer.append_group().unwrap(); // seq 1

		// Group 0 was evicted. Consumer should get group 1.
		let group = consumer.assert_group();
		assert_eq!(group.info.sequence, 1);
	}

	#[tokio::test]
	async fn out_of_order_max_sequence_at_front() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();

		// Arrive out of order: seq 5 first, then 3, then 4.
		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 4 }).unwrap();

		// max_sequence = 5, which is at the front of the VecDeque.
		{
			let state = producer.state.borrow();
			assert_eq!(state.max_sequence, Some(5));
		}

		// Expire all three groups.
		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		// Append seq 6 (becomes new max_sequence).
		producer.append_group().unwrap(); // seq 6

		// Seq 3, 4, 5 are all expired. Seq 5 was the old max_sequence but now 6 is.
		// All old groups are evicted.
		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 6);
			assert!(!state.duplicates.contains(&3));
			assert!(!state.duplicates.contains(&4));
			assert!(!state.duplicates.contains(&5));
			assert!(state.duplicates.contains(&6));
		}
	}

	#[tokio::test]
	async fn max_sequence_at_front_blocks_trim() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();

		// Arrive: seq 5, then seq 3.
		producer.create_group(Group { sequence: 5 }).unwrap();

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		// Seq 3 arrives late; max_sequence is still 5 (at front).
		producer.create_group(Group { sequence: 3 }).unwrap();

		// Seq 5 is max_sequence (protected). Seq 3 is not expired (just created).
		// Nothing should be evicted.
		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
		}

		// Expire seq 3 as well.
		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		// Seq 2 arrives late, triggering eviction.
		producer.create_group(Group { sequence: 2 }).unwrap();

		// Seq 5 is still max_sequence (protected, at front, blocks trim).
		// Seq 3 is expired → tombstoned.
		// Seq 2 is fresh → kept.
		// VecDeque: [Some(5), None, Some(2)]. Leading entry is Some, so offset stays.
		{
			let state = producer.state.borrow();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
			assert!(state.duplicates.contains(&5));
			assert!(!state.duplicates.contains(&3));
			assert!(state.duplicates.contains(&2));
		}

		// Consumer should still be able to read through the hole.
		let mut consumer = producer.consume();
		let group = consumer.assert_group();
		// consume() starts at the last slot (seq 2).
		assert_eq!(group.info.sequence, 2);
	}
}
