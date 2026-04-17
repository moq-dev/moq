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

use crate::{Error, Result, coding};

use super::{Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	task::{Poll, ready},
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
	final_sequence: Option<u64>,
	abort: Option<Error>,
}

impl State {
	/// Find the next non-tombstoned group at or after `index` in arrival order.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	fn poll_recv_group(&self, index: usize, min_sequence: u64) -> Poll<Result<Option<(GroupConsumer, usize)>>> {
		let start = index.saturating_sub(self.offset);
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			if let Some((group, _)) = slot
				&& group.info.sequence >= min_sequence
			{
				return Poll::Ready(Ok(Some((group.consume(), self.offset + i))));
			}
		}

		// TODO once we have drop notifications, check if index == final_sequence.
		if self.final_sequence.is_some() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Scan groups at or after `index` in arrival order, looking for the first with sequence
	/// `>= next_sequence` that has a fully-buffered next frame. Returns the frame plus the
	/// winning slot's absolute index and sequence so the consumer can advance past it.
	fn poll_read_frame(
		&self,
		index: usize,
		next_sequence: u64,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<(bytes::Bytes, usize, u64)>>> {
		let start = index.saturating_sub(self.offset);
		let mut pending_seen = false;
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			let Some((group, _)) = slot else { continue };
			if group.info.sequence < next_sequence {
				continue;
			}

			let mut consumer = group.consume();
			match consumer.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					return Poll::Ready(Ok(Some((frame, self.offset + i, group.info.sequence))));
				}
				Poll::Ready(Ok(None)) => continue,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => {
					pending_seen = true;
					continue;
				}
			}
		}

		// A pending group can still produce a frame even after finish() — finish only
		// blocks new groups at/above final_sequence, not frames on existing groups.
		if pending_seen {
			Poll::Pending
		} else if self.final_sequence.is_some() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn poll_get_group(&self, sequence: u64) -> Poll<Result<Option<GroupConsumer>>> {
		// Search for the group with the matching sequence, skipping tombstones.
		for (group, _) in self.groups.iter().flatten() {
			if group.info.sequence == sequence {
				return Poll::Ready(Ok(Some(group.consume())));
			}
		}

		// Once final_sequence is set, groups at or past it can never exist.
		if let Some(fin) = self.final_sequence
			&& sequence >= fin
		{
			return Poll::Ready(Ok(None));
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}

		Poll::Pending
	}

	fn poll_closed(&self) -> Poll<Result<()>> {
		if self.final_sequence.is_some() {
			Poll::Ready(Ok(()))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
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

	fn poll_finished(&self) -> Poll<Result<u64>> {
		if let Some(fin) = self.final_sequence {
			Poll::Ready(Ok(fin))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}
}

/// A producer for a track, used to create new groups.
pub struct TrackProducer {
	pub info: Track,
	state: conducer::Producer<State>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		Self {
			info,
			state: conducer::Producer::default(),
		}
	}

	/// Create a new group with the given sequence number.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = info.produce();

		let mut state = self.modify()?;
		if let Some(fin) = state.final_sequence
			&& group.info.sequence >= fin
		{
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
		let mut state = self.modify()?;
		let sequence = match state.max_sequence {
			Some(s) => s.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		};
		if let Some(fin) = state.final_sequence
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}

		let group = Group { sequence }.produce();

		let now = tokio::time::Instant::now();
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

	/// Mark the track as finished after the last appended group.
	///
	/// Sets the final sequence to one past the current max_sequence.
	/// No new groups at or above this sequence can be appended.
	/// NOTE: Old groups with lower sequence numbers can still arrive.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.modify()?;
		if state.final_sequence.is_some() {
			return Err(Error::Closed);
		}
		state.final_sequence = Some(match state.max_sequence {
			Some(max) => max.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		});
		Ok(())
	}

	/// Mark the track as finished after the last appended group.
	///
	/// Deprecated: use [`Self::finish`] for this behavior, or
	/// [`Self::finish_at`] to set an explicit final sequence.
	#[deprecated(note = "use finish() or finish_at(sequence) instead")]
	pub fn close(&mut self) -> Result<()> {
		self.finish()
	}

	/// Mark the track as finished at an exact final sequence.
	///
	/// The caller must pass the current max_sequence exactly.
	/// Freezes the final boundary at one past the current max_sequence.
	/// No new groups at or above that sequence can be created.
	/// NOTE: Old groups with lower sequence numbers can still arrive.
	pub fn finish_at(&mut self, sequence: u64) -> Result<()> {
		let mut state = self.modify()?;
		let max = state.max_sequence.ok_or(Error::Closed)?;
		if state.final_sequence.is_some() || sequence != max {
			return Err(Error::Closed);
		}
		state.final_sequence = Some(max.checked_add(1).ok_or(coding::BoundsExceeded)?);
		Ok(())
	}

	/// Abort the track with the given error.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut guard = self.modify()?;

		// Abort all groups still in progress.
		for (group, _) in guard.groups.iter_mut().flatten() {
			// Ignore errors, we don't care if the group was already closed.
			group.abort(err.clone()).ok();
		}

		guard.abort = Some(err);
		guard.close();
		Ok(())
	}

	/// Create a new consumer for the track, starting at the beginning.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
			min_sequence: 0,
			next_sequence: 0,
		}
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		self.state
			.used()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Return true if the track has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.read().is_closed()
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			info: self.info.clone(),
			state: self.state.weak(),
		}
	}

	fn modify(&self) -> Result<conducer::Mut<'_, State>> {
		self.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
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
	state: conducer::Weak<State>,
}

impl TrackWeak {
	pub fn abort(&self, err: Error) {
		let Ok(mut guard) = self.state.write() else { return };

		// Cascade abort to all groups.
		for (group, _) in guard.groups.iter_mut().flatten() {
			group.abort(err.clone()).ok();
		}

		guard.abort = Some(err);
		guard.close();
	}

	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
			min_sequence: 0,
			next_sequence: 0,
		}
	}

	pub async fn unused(&self) -> crate::Result<()> {
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// A consumer for a track, used to read groups.
#[derive(Clone)]
pub struct TrackConsumer {
	pub info: Track,
	state: conducer::Consumer<State>,
	/// Arrival-order cursor used by [`Self::recv_group`].
	index: usize,
	/// Minimum sequence to return from any `recv` method. Set by [`Self::start_at`].
	min_sequence: u64,
	/// One past the highest sequence returned by [`Self::next_group_ordered`].
	/// Used only by that method to skip late arrivals; does not affect [`Self::recv_group`].
	next_sequence: u64,
}

impl TrackConsumer {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&conducer::Ref<'_, State>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for the next group received over the network, in arrival order, without blocking.
	///
	/// Groups may arrive out of order or with gaps due to network conditions.
	/// Use [`Self::next_group_ordered`] if you need groups in sequence order,
	/// skipping those that arrive too late.
	///
	/// Returns `Poll::Ready(Ok(Some(group)))` when a group is available,
	/// `Poll::Ready(Ok(None))` when the track is finished,
	/// `Poll::Ready(Err(e))` when the track has been aborted, or
	/// `Poll::Pending` when no group is available yet.
	pub fn poll_recv_group(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		let Some((consumer, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_group(self.index, self.min_sequence))?)
		else {
			return Poll::Ready(Ok(None));
		};

		self.index = found_index + 1;
		Poll::Ready(Ok(Some(consumer)))
	}

	/// Receive the next group available on this track, in arrival order.
	///
	/// Groups may arrive out of order or with gaps due to network conditions.
	/// Use [`Self::next_group_ordered`] if you need groups in sequence order,
	/// skipping those that arrive too late.
	pub async fn recv_group(&mut self) -> Result<Option<GroupConsumer>> {
		conducer::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Deprecated alias for [`Self::poll_recv_group`].
	#[deprecated(note = "use poll_recv_group for arrival order, or poll_next_group_ordered for sequence order")]
	pub fn poll_next_group(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		self.poll_recv_group(waiter)
	}

	/// Deprecated alias for [`Self::recv_group`].
	#[deprecated(note = "use recv_group for arrival order, or next_group_ordered for sequence order")]
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		self.recv_group().await
	}

	/// A helper that calls [`Self::poll_recv_group`] but only returns groups with a sequence number higher than any previously returned.
	///
	/// NOTE: This will be renamed to `poll_next_group` in the next major version.
	pub fn poll_next_group_ordered(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		loop {
			let Some(group) = ready!(self.poll_recv_group(waiter)?) else {
				return Poll::Ready(Ok(None));
			};
			if group.info.sequence < self.next_sequence {
				// Late arrival; discard and keep looking.
				continue;
			}
			self.next_sequence = group.info.sequence.saturating_add(1);
			return Poll::Ready(Ok(Some(group)));
		}
	}

	/// Return the next group with a strictly-greater sequence number than the last returned.
	///
	/// Groups that arrive late (with a sequence number at or below the last one returned)
	/// are silently skipped.
	///
	/// NOTE: This will be renamed to `next_group` in the next major version.
	pub async fn next_group_ordered(&mut self) -> Result<Option<GroupConsumer>> {
		conducer::wait(|waiter| self.poll_next_group_ordered(waiter)).await
	}

	/// A helper that calls [`Self::poll_next_group_ordered`] and returns its first frame,
	/// skipping the rest of the group. Intended for single-frame groups (see
	/// [`TrackProducer::write_frame`]).
	pub fn poll_read_frame(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<bytes::Bytes>>> {
		let lower = self.min_sequence.max(self.next_sequence);
		let Some((frame, found_index, sequence)) =
			ready!(self.poll(waiter, |state| { state.poll_read_frame(self.index, lower, waiter) })?)
		else {
			return Poll::Ready(Ok(None));
		};

		self.index = found_index + 1;
		self.next_sequence = sequence.saturating_add(1);
		Poll::Ready(Ok(Some(frame)))
	}

	/// Read a single full frame from the next group in sequence order.
	///
	/// See [`Self::poll_read_frame`] for semantics.
	pub async fn read_frame(&mut self) -> Result<Option<bytes::Bytes>> {
		conducer::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Poll for the group with the given sequence, without blocking.
	pub fn poll_get_group(&self, waiter: &conducer::Waiter, sequence: u64) -> Poll<Result<Option<GroupConsumer>>> {
		self.poll(waiter, |state| state.poll_get_group(sequence))
	}

	/// Block until the group with the given sequence is available.
	///
	/// Returns None if the group is not in the cache and a newer group exists.
	pub async fn get_group(&self, sequence: u64) -> Result<Option<GroupConsumer>> {
		conducer::wait(|waiter| self.poll_get_group(waiter, sequence)).await
	}

	/// Poll for track closure, without blocking.
	pub fn poll_closed(&self, waiter: &conducer::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_closed())
	}

	/// Block until the track is closed.
	///
	/// Returns Ok() is the track was cleanly finished.
	pub async fn closed(&self) -> Result<()> {
		conducer::wait(|waiter| self.poll_closed(waiter)).await
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Poll for the total number of groups in the track.
	pub fn poll_finished(&mut self, waiter: &conducer::Waiter) -> Poll<Result<u64>> {
		self.poll(waiter, |state| state.poll_finished())
	}

	/// Block until the track is finished, returning the total number of groups.
	pub async fn finished(&mut self) -> Result<u64> {
		conducer::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Start the consumer at the specified sequence.
	pub fn start_at(&mut self, sequence: u64) {
		self.min_sequence = sequence;
	}

	/// Return the latest sequence number in the track.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().max_sequence
	}

	/// Upgrade this consumer back to a [TrackProducer] sharing the same state.
	///
	/// This enables zero-copy track sharing between broadcasts: subscribe to a
	/// track, then [`crate::BroadcastProducer::insert_track`] the producer into another
	/// broadcast. Both broadcasts serve the same underlying track data with no
	/// forwarding overhead.
	///
	/// # Shared Ownership
	///
	/// The returned producer shares state with the original track. Mutations
	/// (appending groups, finishing, aborting) through either producer affect
	/// all consumers of the track. The returned producer keeps the track alive
	/// (prevents auto-close) as long as it exists, even if the original producer
	/// is dropped.
	pub fn produce(&self) -> Result<TrackProducer> {
		let state = self
			.state
			.produce()
			.ok_or_else(|| self.state.read().abort.clone().unwrap_or(Error::Dropped))?;
		Ok(TrackProducer {
			info: self.info.clone(),
			state,
		})
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl TrackConsumer {
	pub fn assert_group(&mut self) -> GroupConsumer {
		self.recv_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_no_group(&mut self) {
		assert!(
			self.recv_group().now_or_never().is_none(),
			"recv_group would not have blocked"
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
			let state = producer.state.read();
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
			let state = producer.state.read();
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
			let state = producer.state.read();
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
			let state = producer.state.read();
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
			let state = producer.state.read();
			assert_eq!(state.max_sequence, Some(5));
		}

		// Expire all three groups.
		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		// Append seq 6 (becomes new max_sequence).
		producer.append_group().unwrap(); // seq 6

		// Seq 3, 4, 5 are all expired. Seq 5 was the old max_sequence but now 6 is.
		// All old groups are evicted.
		{
			let state = producer.state.read();
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
			let state = producer.state.read();
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
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
			assert!(state.duplicates.contains(&5));
			assert!(!state.duplicates.contains(&3));
			assert!(state.duplicates.contains(&2));
		}

		// Consumer should still be able to read through the hole.
		let mut consumer = producer.consume();
		let group = consumer.assert_group();
		// consume() starts at index 0, first non-tombstoned group is seq 5.
		assert_eq!(group.info.sequence, 5);
	}

	#[test]
	fn append_finish_cannot_be_rewritten() {
		let mut producer = Track::new("test").produce();

		// Finishing an empty track is valid (fin = 0, total groups = 0).
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn finish_after_groups() {
		let mut producer = Track::new("test").produce();

		producer.append_group().unwrap();
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn insert_finish_validates_sequence_and_freezes_to_max() {
		let mut producer = Track::new("test").produce();
		producer.create_group(Group { sequence: 5 }).unwrap();

		assert!(producer.finish_at(4).is_err());
		assert!(producer.finish_at(10).is_err());
		assert!(producer.finish_at(5).is_ok());

		{
			let state = producer.state.read();
			assert_eq!(state.final_sequence, Some(6));
		}

		assert!(producer.finish_at(5).is_err());
		assert!(producer.create_group(Group { sequence: 4 }).is_ok());
		assert!(producer.create_group(Group { sequence: 5 }).is_err());
	}

	#[tokio::test]
	async fn recv_group_finishes_without_waiting_for_gaps() {
		let mut producer = Track::new("test").produce();
		producer.create_group(Group { sequence: 1 }).unwrap();
		producer.finish_at(1).unwrap();

		let mut consumer = producer.consume();
		assert_eq!(consumer.assert_group().info.sequence, 1);

		let done = consumer
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track should finish without waiting for gaps");
	}

	#[tokio::test]
	async fn next_group_ordered_skips_late_arrivals() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		// Seq 5 arrives first.
		producer.create_group(Group { sequence: 5 }).unwrap();
		let group = consumer
			.next_group_ordered()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.info.sequence, 5);

		// Seq 3 arrives late — skipped because 3 <= 5.
		producer.create_group(Group { sequence: 3 }).unwrap();
		// Seq 4 arrives late — also skipped.
		producer.create_group(Group { sequence: 4 }).unwrap();
		// Seq 7 arrives — returned.
		producer.create_group(Group { sequence: 7 }).unwrap();

		let group = consumer
			.next_group_ordered()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.info.sequence, 7);

		// No more groups — would block.
		assert!(
			consumer.next_group_ordered().now_or_never().is_none(),
			"should block waiting for a higher sequence"
		);
	}

	#[tokio::test]
	async fn next_group_ordered_returns_arrivals_in_order() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		// Seq 3 arrives first, then seq 5 — both should be returned in arrival order.
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 5 }).unwrap();

		let group = consumer
			.next_group_ordered()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.info.sequence, 3);

		let group = consumer
			.next_group_ordered()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.info.sequence, 5);
	}

	#[tokio::test]
	async fn recv_group_after_next_group_ordered_sees_late_arrivals() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();

		// Ordered returns seq 5 and advances its internal cursor past it.
		let group = consumer
			.next_group_ordered()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.info.sequence, 5);

		// Intermixing: recv_group on the same consumer still returns the late seq 3.
		// The ordered cursor is separate from the recv_group filter.
		assert_eq!(consumer.assert_group().info.sequence, 3);
	}

	#[tokio::test]
	async fn read_frame_returns_single_frame_per_group() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		producer.write_frame(b"hello".as_slice()).unwrap();
		producer.write_frame(b"world".as_slice()).unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"hello");

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"world");
	}

	#[tokio::test]
	async fn read_frame_skips_stalled_group_for_newer_ready_frame() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		// Seq 3: group open, no frame yet (stalled).
		let _stalled = producer.create_group(Group { sequence: 3 }).unwrap();
		// Seq 5: fully-written group with a frame.
		let mut g5 = producer.create_group(Group { sequence: 5 }).unwrap();
		g5.write_frame(bytes::Bytes::from_static(b"later")).unwrap();
		g5.finish().unwrap();

		// read_frame should not block on the stalled seq 3 — it returns seq 5's frame.
		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block on stalled earlier group")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"later");
	}

	#[tokio::test]
	async fn read_frame_discards_rest_of_multi_frame_group() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		// Group 0 has two frames; only the first is returned.
		let mut g0 = producer.create_group(Group { sequence: 0 }).unwrap();
		g0.write_frame(bytes::Bytes::from_static(b"one")).unwrap();
		g0.write_frame(bytes::Bytes::from_static(b"two")).unwrap();
		g0.finish().unwrap();

		// Group 1 is a normal single-frame group.
		producer.write_frame(b"next".as_slice()).unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"one");

		// The second frame of group 0 is discarded; the next read jumps to group 1.
		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"next");
	}

	#[tokio::test]
	async fn read_frame_waits_for_pending_group_after_finish() {
		// finish() sets final_sequence, but groups already created with lower sequences
		// can still produce frames. read_frame must not return None prematurely.
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		let mut g0 = producer.create_group(Group { sequence: 0 }).unwrap();
		producer.finish().unwrap();

		// Track is finished but group 0 has no frame yet — must block, not return None.
		assert!(
			consumer.read_frame().now_or_never().is_none(),
			"read_frame must block on a pending group even after finish()"
		);

		// A late frame on the pending group is still delivered.
		g0.write_frame(bytes::Bytes::from_static(b"late")).unwrap();
		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block once a frame is written")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"late");
	}

	#[tokio::test]
	async fn read_frame_respects_start_at() {
		// start_at sets min_sequence; read_frame must skip groups below it even though
		// next_sequence is still 0.
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();
		consumer.start_at(5);

		// Seq 3 has a frame but is below min_sequence — must be skipped.
		let mut g3 = producer.create_group(Group { sequence: 3 }).unwrap();
		g3.write_frame(bytes::Bytes::from_static(b"skip-me")).unwrap();
		g3.finish().unwrap();

		let mut g5 = producer.create_group(Group { sequence: 5 }).unwrap();
		g5.write_frame(bytes::Bytes::from_static(b"keep")).unwrap();
		g5.finish().unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"keep");
	}

	#[tokio::test]
	async fn read_frame_returns_none_when_finished() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		producer.write_frame(b"only".as_slice()).unwrap();
		producer.finish().unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"only");

		let done = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none());
	}

	#[tokio::test]
	async fn get_group_finishes_without_waiting_for_gaps() {
		let mut producer = Track::new("test").produce();
		producer.create_group(Group { sequence: 1 }).unwrap();
		producer.finish_at(1).unwrap();

		let consumer = producer.consume();
		// get_group(0) blocks because group 0 is below final_sequence and could still arrive.
		assert!(
			consumer.get_group(0).now_or_never().is_none(),
			"sequence below fin should block (group could still arrive)"
		);
		assert!(
			consumer
				.get_group(2)
				.now_or_never()
				.expect("sequence at-or-after fin should resolve")
				.expect("should not error")
				.is_none(),
			"sequence at-or-after fin should not exist"
		);
	}

	#[test]
	fn append_group_returns_bounds_exceeded_on_sequence_overflow() {
		let mut producer = Track::new("test").produce();
		{
			let mut state = producer.state.write().ok().unwrap();
			state.max_sequence = Some(u64::MAX);
		}

		assert!(matches!(producer.append_group(), Err(Error::BoundsExceeded(_))));
	}

	#[tokio::test]
	async fn consumer_produce() {
		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap();

		let consumer = producer.consume();

		// Upgrade consumer back to producer — shared state.
		let got = consumer.produce().expect("should produce");
		assert!(got.is_clone(&producer), "should be the same track");

		// Writing through the upgraded producer is visible to new consumers.
		got.clone().append_group().unwrap();
		let mut sub = producer.consume();
		sub.assert_group(); // group 0
		sub.assert_group(); // group 1, written via upgraded producer
	}

	#[tokio::test]
	async fn consumer_produce_after_drop() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		drop(producer);

		// Original producer dropped. Consumer's produce() should fail
		// because there are no remaining producers.
		let err = consumer.produce();
		assert!(matches!(err, Err(Error::Dropped)), "expected Dropped");
	}

	#[tokio::test]
	async fn consumer_produce_after_abort() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();
		producer.abort(Error::Cancel).unwrap();
		drop(producer);

		// Track was aborted — produce() should return the abort error, not Dropped.
		let err = consumer.produce();
		assert!(matches!(err, Err(Error::Cancel)), "expected Cancel");
	}

	#[tokio::test]
	async fn consumer_produce_keeps_alive() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let upgraded = consumer.produce().expect("should produce");
		drop(producer);

		// Channel still open because upgraded producer exists.
		assert!(consumer.closed().now_or_never().is_none(), "should not be closed");
		drop(upgraded);

		// Now it should close.
		assert!(consumer.closed().now_or_never().is_some(), "should be closed");
	}
}
