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

use super::{Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	task::{Poll, ready},
	time::Duration,
};

/// Groups older than this are evicted from the track cache (unless they are the max_sequence group).
// TODO: Replace with a configurable cache size.
const MAX_GROUP_AGE: Duration = Duration::from_secs(30);

/// A track is a collection of groups, identified by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	pub name: String,
}

impl Track {
	pub fn new<T: Into<String>>(name: T) -> Self {
		Self { name: name.into() }
	}

	pub fn produce(self) -> TrackProducer {
		TrackProducer::new(self)
	}
}

/// Subscription preferences for a subscription or producer cap.
///
/// Describes how groups should be delivered: priority, ordering, latency bounds,
/// and the range of groups requested.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subscription {
	pub priority: u8,
	pub ordered: bool,
	/// Maximum cache/latency. `Duration::ZERO` means unlimited.
	pub max_latency: Duration,
	/// First group sequence to deliver. `None` means no preference.
	/// Use [`TrackSubscriber::update`] to set a concrete value once `latest()` is known.
	pub start: Option<u64>,
	/// Last group sequence to deliver. `None` means no preference (live).
	pub end: Option<u64>,
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

	/// Per-subscriber subscription values, read by the producer for aggregation.
	subscriptions: Vec<conducer::Consumer<Subscription>>,
}

impl State {
	/// Find the next non-tombstoned group at or after `index`.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	fn poll_next_group(&self, index: usize, min_sequence: u64) -> Poll<Result<Option<(GroupConsumer, usize)>>> {
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

	/// Compute the aggregate subscription from all active (non-closed) subscribers.
	///
	/// Aggregation rules:
	/// - `priority`: max across all subscribers (highest-priority wins).
	/// - `ordered`: AND of all subscribers (true only if every subscriber requests ordered).
	/// - `max_latency`: max across subscribers, with `Duration::ZERO` meaning unlimited (ZERO wins).
	/// - `start`/`end`: min/max of concrete values; `None` means no preference (deferred to others).
	///
	/// Returns `None` if there are no active subscriptions.
	fn subscription(&self) -> Option<Subscription> {
		if self.subscriptions.is_empty() {
			return None;
		}

		// Read each subscriber's current subscription value into owned copies.
		let subs: Vec<Subscription> = self
			.subscriptions
			.iter()
			.filter_map(|c| {
				let r = c.read();
				if r.is_closed() { None } else { Some(r.clone()) }
			})
			.collect();

		if subs.is_empty() {
			return None;
		}

		let priority = subs.iter().map(|s| s.priority).max().unwrap();

		// ordered is true only if ALL subscribers want ordered.
		let ordered = subs.iter().all(|s| s.ordered);

		// max_latency: max across subscriptions. ZERO = unlimited wins.
		let max_latency = subs
			.iter()
			.map(|s| s.max_latency)
			.reduce(|a, b| {
				if a.is_zero() || b.is_zero() {
					Duration::ZERO
				} else {
					a.max(b)
				}
			})
			.unwrap();

		// start: min across all concrete values. None = no preference (defer to others).
		let start = subs
			.iter()
			.map(|s| s.start)
			.reduce(|a, b| match (a, b) {
				(Some(a), Some(b)) => Some(a.min(b)),
				(Some(a), None) | (None, Some(a)) => Some(a),
				(None, None) => None,
			})
			.unwrap();

		// end: max across all concrete values. None = no end (defer to others).
		let end = subs
			.iter()
			.map(|s| s.end)
			.reduce(|a, b| match (a, b) {
				(Some(a), Some(b)) => Some(a.max(b)),
				(Some(a), None) | (None, Some(a)) => Some(a),
				(None, None) => None,
			})
			.unwrap();

		Some(Subscription {
			priority,
			ordered,
			max_latency,
			start,
			end,
		})
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

	/// Check if the track is "ready" for a subscriber: has at least one group,
	/// is finished (with 0+ groups), or is aborted.
	fn poll_ready(&self) -> Poll<Result<()>> {
		if self.max_sequence.is_some() || self.final_sequence.is_some() {
			Poll::Ready(Ok(()))
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
	/// The last aggregate subscription returned by [`Self::subscription`].
	prev_subscription: Option<Subscription>,
}

impl TrackProducer {
	pub fn new(info: Track) -> Self {
		Self {
			info,
			state: conducer::Producer::default(),
			prev_subscription: None,
		}
	}

	/// Create a new group with the given sequence number.
	///
	/// If a group with the same sequence already exists but was aborted (e.g. due to a
	/// cancelled subscription), it will be replaced with a tombstone and the new group
	/// pushed to the end. Successfully completed groups return `Err(Error::Duplicate)`.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = info.produce();

		let mut state = self.modify()?;
		if let Some(fin) = state.final_sequence
			&& group.info.sequence >= fin
		{
			return Err(Error::Closed);
		}

		if !state.duplicates.insert(group.info.sequence) {
			// Sequence exists -- check if the existing group was aborted.
			for slot in state.groups.iter_mut() {
				if let Some((existing, _)) = slot {
					if existing.info.sequence == group.info.sequence {
						if !existing.is_aborted() {
							return Err(Error::Duplicate);
						}
						// Tombstone the old entry and push the new group at the end.
						*slot = None;
						break;
					}
				}
			}

			let now = tokio::time::Instant::now();
			state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.info.sequence));
			state.groups.push_back(Some((group.clone(), now)));
			state.evict_expired(now);
			return Ok(group);
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
			Some(s) => s.checked_add(1).ok_or(Error::BoundsExceeded)?,
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
			Some(max) => max.checked_add(1).ok_or(Error::BoundsExceeded)?,
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
		state.final_sequence = Some(max.checked_add(1).ok_or(Error::BoundsExceeded)?);
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
		}
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state
			.unused()
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

	/// Poll for changes to the aggregate subscription.
	///
	/// Returns `Ready(sub)` when the aggregate differs from the last value returned.
	/// Returns `Ready(None)` when no subscriptions are active (or the track is closed).
	pub fn poll_subscription(&mut self, waiter: &conducer::Waiter) -> Poll<Option<Subscription>> {
		let prev = self.prev_subscription.as_ref();
		match self.state.poll(waiter, |state| {
			// Remove closed subscription consumers.
			state.subscriptions.retain(|c| !c.read().is_closed());

			// Also poll each subscription consumer so the waiter is registered
			// on inner changes too.
			for sub in &state.subscriptions {
				// Register the waiter on each subscription's conducer channel.
				let _ = sub.poll(waiter, |_| Poll::<()>::Pending);
			}

			let current = state.subscription();
			if current.as_ref() != prev {
				Poll::Ready(current)
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(Ok(sub)) => {
				self.prev_subscription = sub.clone();
				Poll::Ready(sub)
			}
			Poll::Ready(Err(_)) => {
				self.prev_subscription = None;
				Poll::Ready(None)
			}
			Poll::Pending => Poll::Pending,
		}
	}

	/// Block until the aggregate subscription changes.
	///
	/// Returns `None` when all subscriptions are dropped or the track is closed.
	pub async fn subscription(&mut self) -> Option<Subscription> {
		conducer::wait(|waiter| self.poll_subscription(waiter)).await
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
			prev_subscription: self.prev_subscription.clone(),
		}
	}
}

impl<T: Into<Track>> From<T> for TrackProducer {
	fn from(info: T) -> Self {
		TrackProducer::new(info.into())
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
		}
	}
}

/// Iterates groups from a track while managing this subscriber's subscription lifecycle.
///
/// Created via [`TrackConsumer::subscribe`]. Registers a subscription in the
/// shared state on creation; automatically removes it on drop.
/// Blocks in `subscribe()` until the first group exists (or finish/abort).
pub struct TrackSubscriber {
	pub info: Track,
	state: conducer::Consumer<State>,
	sub: conducer::Producer<Subscription>,
	index: usize,
}

impl TrackSubscriber {
	/// Poll whether the track is "ready": has at least one group, is finished, or aborted.
	pub fn poll_ready(&self, waiter: &conducer::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_ready())
	}

	/// Wait until the track is ready (has at least one group, is finished, or aborted).
	pub async fn ready(&self) -> Result<()> {
		conducer::wait(|waiter| self.poll_ready(waiter)).await
	}

	/// Receive the next group respecting the subscription start/end range.
	pub async fn recv_group(&mut self) -> Result<Option<GroupConsumer>> {
		conducer::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	pub fn poll_recv_group(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		let sub = self.sub.read();
		let min_sequence = sub.start.unwrap_or(0);
		let end = sub.end;
		drop(sub);

		let Some((consumer, found_index)) =
			ready!(self.poll(waiter, |state| { state.poll_next_group(self.index, min_sequence) })?)
		else {
			return Poll::Ready(Ok(None));
		};

		// Check end bound.
		if let Some(end) = end {
			if consumer.info.sequence > end {
				return Poll::Ready(Ok(None));
			}
		}

		self.index = found_index + 1;
		Poll::Ready(Ok(Some(consumer)))
	}

	/// Update this subscription's preferences.
	pub fn update(&mut self, sub: Subscription) {
		if let Ok(mut guard) = self.sub.write() {
			*guard = sub;
		}
	}

	/// Return the latest sequence number in the track.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().max_sequence
	}

	/// The current subscription preferences.
	pub fn subscription(&self) -> Subscription {
		self.sub.read().clone()
	}

	/// Poll for track closure, without blocking.
	pub fn poll_closed(&self, waiter: &conducer::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_closed())
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		conducer::wait(|waiter| self.poll_closed(waiter)).await
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&conducer::Ref<'_, State>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}
}

impl Drop for TrackSubscriber {
	fn drop(&mut self) {
		// Close the subscription producer so the TrackProducer knows to remove it.
		// The producer's poll_subscription will clean up closed consumers.
		let _ = self.sub.close();
	}
}

/// A consumer for a track, used to read groups.
#[derive(Clone)]
pub struct TrackConsumer {
	pub info: Track,
	state: conducer::Consumer<State>,
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

	/// Removed: Use [`TrackSubscriber::recv_group`] instead.
	#[deprecated(note = "Use TrackSubscriber::recv_group instead")]
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		unimplemented!("Use TrackSubscriber::recv_group instead")
	}

	/// Removed: Use [`TrackSubscriber::poll_recv_group`] instead.
	#[deprecated(note = "Use TrackSubscriber::poll_recv_group instead")]
	pub fn poll_next_group(&mut self, _waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		unimplemented!("Use TrackSubscriber::poll_recv_group instead")
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

	/// Register a subscription and return a [`TrackSubscriber`] immediately.
	///
	/// The subscriber can be used right away, but callers may want to call
	/// [`TrackSubscriber::ready`] or poll [`TrackSubscriber::poll_ready`] to
	/// wait until the first group exists (or finish/abort).
	///
	/// Dropping the subscriber removes the subscription from the aggregate.
	pub fn subscribe(&self, sub: Subscription) -> Result<TrackSubscriber> {
		// Create a conducer channel for this subscription's preferences.
		let sub_producer = conducer::Producer::new(sub);
		let sub_consumer = sub_producer.consume();

		{
			// Use weak write access to insert the subscription consumer.
			// If the channel is closed (track finished/dropped), skip — the subscriber
			// can still read existing groups without an active subscription.
			let weak = self.state.weak();
			if let Ok(mut state) = weak.write() {
				state.subscriptions.push(sub_consumer);
			}
		}

		Ok(TrackSubscriber {
			info: self.info.clone(),
			state: self.state.clone(),
			sub: sub_producer,
			index: 0,
		})
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			info: self.info.clone(),
			state: self.state.weak(),
		}
	}

	/// Return the latest sequence number in the track.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().max_sequence
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[allow(deprecated)]
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
			"next_group would not have blocked"
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

	pub fn assert_subscribe(&self) -> TrackSubscriber {
		self.subscribe(Subscription::default())
			.expect("subscribe should not have errored")
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
	async fn subscriber_skips_evicted_groups() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap(); // seq 0

		let consumer = producer.consume();
		let mut subscriber = consumer.subscribe(Subscription::default()).unwrap();

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;
		producer.append_group().unwrap(); // seq 1

		// Group 0 was evicted. Subscriber should get group 1.
		let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
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

		// Subscriber should still be able to read through the hole.
		let consumer = producer.consume();
		let mut subscriber = consumer.subscribe(Subscription::default()).unwrap();
		let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
		// subscribe() starts at index 0, first non-tombstoned group is seq 5.
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

		let consumer = producer.consume();
		let mut subscriber = consumer.subscribe(Subscription::default()).unwrap();
		assert_eq!(
			subscriber
				.recv_group()
				.now_or_never()
				.expect("should not block")
				.expect("would have errored")
				.expect("track was closed")
				.info
				.sequence,
			1
		);

		let done = subscriber
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track should finish without waiting for gaps");
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

		assert!(matches!(producer.append_group(), Err(Error::BoundsExceeded)));
	}

	#[tokio::test]
	async fn create_group_replaces_aborted() {
		let mut producer = Track::new("test").produce();

		// Create and abort a group.
		let mut group = producer.create_group(Group { sequence: 5 }).unwrap();
		group.abort(Error::Cancel).unwrap();

		// Creating the same group again should succeed (replaces aborted).
		let group2 = producer.create_group(Group { sequence: 5 });
		assert!(group2.is_ok(), "should replace aborted group");

		// Creating again on a non-aborted group should fail.
		let group3 = producer.create_group(Group { sequence: 5 });
		assert!(
			matches!(group3, Err(Error::Duplicate)),
			"should not replace active group"
		);
	}

	#[tokio::test]
	async fn create_group_does_not_replace_finished() {
		let mut producer = Track::new("test").produce();

		// Create and finish a group.
		let mut group = producer.create_group(Group { sequence: 5 }).unwrap();
		group.finish().unwrap();

		// Should fail because the group finished successfully.
		let result = producer.create_group(Group { sequence: 5 });
		assert!(matches!(result, Err(Error::Duplicate)));
	}

	#[tokio::test]
	async fn subscribe_unblocks_on_first_group() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		// ready() blocks until a group exists.
		let handle = tokio::spawn(async move {
			let sub = consumer.subscribe(Subscription::default()).unwrap();
			sub.ready().await?;
			Ok::<_, Error>(sub)
		});

		// Yield to let ready() start waiting.
		tokio::task::yield_now().await;

		// Write a group to unblock ready().
		producer.append_group().unwrap();

		let subscriber = handle.await.unwrap().unwrap();
		assert_eq!(subscriber.latest(), Some(0));
	}

	#[tokio::test]
	async fn subscribe_unblocks_on_finish() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let handle = tokio::spawn(async move {
			let sub = consumer.subscribe(Subscription::default()).unwrap();
			sub.ready().await?;
			Ok::<_, Error>(sub)
		});

		tokio::task::yield_now().await;

		// Finishing without any groups should also unblock ready().
		producer.finish().unwrap();

		let subscriber = handle.await.unwrap().unwrap();
		assert!(subscriber.latest().is_none());
	}

	#[tokio::test]
	async fn subscribe_unblocks_on_abort() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let handle = tokio::spawn(async move {
			let sub = consumer.subscribe(Subscription::default()).unwrap();
			sub.ready().await
		});

		tokio::task::yield_now().await;

		// Aborting should unblock ready() with an error.
		producer.abort(Error::Cancel).unwrap();

		let result = handle.await.unwrap();
		assert!(result.is_err(), "ready() should return error on abort");
	}

	#[tokio::test]
	async fn subscribe_returns_immediately_if_group_exists() {
		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap();

		let consumer = producer.consume();

		// subscribe is sync now; ready() should not block since a group already exists.
		let subscriber = consumer.subscribe(Subscription::default()).expect("should not error");
		subscriber
			.ready()
			.now_or_never()
			.expect("should not block")
			.expect("should not error");

		assert_eq!(subscriber.latest(), Some(0));
	}

	#[tokio::test]
	async fn subscribe_returns_immediately_if_finished() {
		let mut producer = Track::new("test").produce();
		producer.finish().unwrap();

		let consumer = producer.consume();

		// subscribe is sync now; ready() should not block since the track is finished.
		let subscriber = consumer.subscribe(Subscription::default()).expect("should not error");
		subscriber
			.ready()
			.now_or_never()
			.expect("should not block")
			.expect("should not error");

		assert!(subscriber.latest().is_none());
	}
}
