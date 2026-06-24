//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackConsumer] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequence number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [TrackProducer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackConsumer] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Transport is to not block on them.
//! Streams will be cached for a potentially limited duration added to the unreliable nature.
//! A cloned [TrackConsumer] will receive a copy of all new streams going forward (fanout).
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use crate::{Error, Result, coding};

use super::cache::{self, Cache};
use super::{Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	task::{Poll, ready},
};

/// A track is a collection of groups, delivered out-of-order until expired.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
	/// Identifier within a broadcast. Unique per [`crate::Broadcast`].
	pub name: String,
	/// Delivery priority. Higher values preempt lower ones when bandwidth is constrained.
	pub priority: u8,
}

impl Track {
	/// Create a track with the given name and default priority (`0`).
	pub fn new<T: Into<String>>(name: T) -> Self {
		Self {
			name: name.into(),
			priority: 0,
		}
	}

	/// Consume this [`Track`] to create a producer that owns its metadata.
	pub fn produce(self) -> TrackProducer {
		TrackProducer::new(self)
	}
}

/// A cached group plus its registration in the shared [`Cache`], if any. A group is registered
/// only once it stops being the latest; the latest group is never handed to the cache.
struct Cached {
	group: GroupProducer,
	token: Option<cache::Token>,
}

#[derive(Default)]
struct State {
	/// Groups in arrival order. `None` entries are tombstones for evicted groups.
	groups: VecDeque<Option<Cached>>,
	duplicates: HashSet<u64>,
	offset: usize,
	max_sequence: Option<u64>,
	final_sequence: Option<u64>,
	abort: Option<Error>,

	/// Shared RAM cache governing retention of non-latest groups. `None` (the default) keeps
	/// only the latest group: every superseded group is dropped at once.
	cache: Option<Cache>,
}

impl State {
	/// Find the next non-tombstoned group at or after `index` in arrival order.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	fn poll_recv_group(&self, index: usize, min_sequence: u64) -> Poll<Result<Option<(GroupConsumer, usize)>>> {
		let start = index.saturating_sub(self.offset);
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			if let Some(cached) = slot
				&& !cached.group.is_aborted()
				&& cached.group.sequence >= min_sequence
			{
				self.touch(cached);
				return Poll::Ready(Ok(Some((cached.group.consume(), self.offset + i))));
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
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<(bytes::Bytes, usize, u64)>>> {
		let start = index.saturating_sub(self.offset);
		let mut pending_seen = false;
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			let Some(cached) = slot else { continue };
			let group = &cached.group;
			if group.is_aborted() || group.sequence < next_sequence {
				continue;
			}

			let mut consumer = group.consume();
			match consumer.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					return Poll::Ready(Ok(Some((frame, self.offset + i, group.sequence))));
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
		// Search for the group with the matching sequence, skipping tombstones and evicted groups.
		for cached in self.groups.iter().flatten() {
			if cached.group.sequence == sequence && !cached.group.is_aborted() {
				self.touch(cached);
				return Poll::Ready(Ok(Some(cached.group.consume())));
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

	/// Apply the retention policy after a group is added, then trim evicted slots.
	///
	/// The current max_sequence group is always kept (a live subscriber must be able to grab
	/// it), so it is never handed to the shared cache. Every other live group is either dropped
	/// at once (no cache: latest-only) or registered with the cache, which evicts by the shared
	/// byte/age budget across all tracks. A group the cache has aborted (here or via another
	/// track's insert) is tombstoned.
	fn retain(&mut self, now: tokio::time::Instant) {
		let max_sequence = self.max_sequence;

		for slot in self.groups.iter_mut() {
			let Some(cached) = slot else { continue };

			// Never evict the current latest group.
			if Some(cached.group.sequence) == max_sequence {
				continue;
			}

			match &self.cache {
				None => {
					// Latest-only: a superseded group is dropped immediately.
					self.duplicates.remove(&cached.group.sequence);
					cached.group.abort(Error::Old).ok();
					*slot = None;
				}
				Some(cache) => {
					// Hand the group to the shared budget the first time it is superseded.
					if cached.token.is_none() {
						let bytes = cached.group.cached_size();
						cached.token = Some(cache.insert(cached.group.clone(), bytes, now));
					}
				}
			}
		}

		// Run age eviction on the shared budget now, so an active track ages out stale groups
		// (its own and its peers') even when no new group needed registering this round.
		if let Some(cache) = &self.cache {
			cache.evict(now);
		}

		// The shared cache may have aborted some of our (or another track's) groups; tombstone
		// any that are now aborted so consumers skip them and the budget bookkeeping matches.
		for slot in self.groups.iter_mut() {
			if let Some(cached) = slot
				&& cached.group.is_aborted()
				&& Some(cached.group.sequence) != max_sequence
			{
				self.duplicates.remove(&cached.group.sequence);
				*slot = None;
			}
		}

		// Trim leading tombstones to advance the offset.
		while let Some(None) = self.groups.front() {
			self.groups.pop_front();
			self.offset += 1;
		}
	}

	/// Record a read of a cached group as a wall-clock access, so the shared cache treats it as
	/// recently used. A no-op for the latest group (never in the cache) and when no cache is set.
	fn touch(&self, cached: &Cached) {
		if let (Some(cache), Some(token)) = (&self.cache, cached.token) {
			cache.touch(token, tokio::time::Instant::now());
		}
	}

	/// Drop this track's entries from the shared cache, freeing the budget without aborting the
	/// groups (the track is going away on its own terms). Called when the track is cleared.
	fn release_cache(&mut self) {
		if let Some(cache) = &self.cache {
			for cached in self.groups.iter_mut().flatten() {
				if let Some(token) = cached.token.take() {
					cache.remove(token);
				}
			}
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
	info: Track,
	state: kio::Producer<State>,
}

impl std::ops::Deref for TrackProducer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl TrackProducer {
	/// Build a producer for the given track metadata. Prefer [`Track::produce`].
	pub fn new(info: Track) -> Self {
		Self {
			info,
			state: kio::Producer::default(),
		}
	}

	/// Attach a shared [`Cache`] governing how much of this track's history is retained.
	///
	/// Without a cache, the track keeps only its latest group (a superseded group is dropped at
	/// once). With one, superseded groups are retained in RAM up to the cache's shared byte and
	/// age budget, evicted least-recently-accessed first. Clone the same [`Cache`] across tracks
	/// to share one budget. Set this before producing groups; it takes effect on the next group.
	/// Returns `self` for chaining.
	pub fn with_cache(self, cache: Cache) -> Self {
		if let Ok(mut state) = self.state.write() {
			state.cache = Some(cache);
		}
		self
	}

	/// Create a new group with the given sequence number.
	pub fn create_group(&mut self, info: Group) -> Result<GroupProducer> {
		let group = info.produce();

		let mut state = self.modify()?;
		if let Some(fin) = state.final_sequence
			&& group.sequence >= fin
		{
			return Err(Error::Closed);
		}

		if !state.duplicates.insert(group.sequence) {
			return Err(Error::Duplicate);
		}

		let now = tokio::time::Instant::now();
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.sequence));
		state.groups.push_back(Some(Cached {
			group: group.clone(),
			token: None,
		}));
		state.retain(now);

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
		state.groups.push_back(Some(Cached {
			group: group.clone(),
			token: None,
		}));
		state.retain(now);

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
	///
	/// Drops the cached groups so a stale [`TrackConsumer`] can't pin them (and
	/// their frame buffers) in memory forever. Consumers that haven't drained yet
	/// surface the abort error instead of the leftover cache. Child groups are
	/// independent: a consumer that already pulled a [`GroupConsumer`] keeps its
	/// own handle and can finish reading it.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut guard = self.modify()?;
		guard.release_cache();
		guard.abort = Some(err);
		guard.groups.clear();
		guard.duplicates.clear();
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

	/// Block until the track is closed or aborted, returning the cause.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
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

	fn modify(&self) -> Result<kio::Mut<'_, State>> {
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

impl Drop for TrackProducer {
	fn drop(&mut self) {
		// The last producer going away without finishing is an abrupt teardown:
		// release the cached groups so a stale consumer can't pin them (and their
		// frame buffers) forever, the same as an explicit abort. A cleanly
		// finished track keeps its cache so consumers can still drain it.
		if !self.state.is_last() {
			return;
		}
		if let Ok(mut state) = self.state.write()
			&& state.final_sequence.is_none()
		{
			state.release_cache();
			state.groups.clear();
			state.duplicates.clear();
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
	pub(crate) info: Track,
	state: kio::Weak<State>,
}

impl TrackWeak {
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
	info: Track,
	state: kio::Consumer<State>,
	/// Arrival-order cursor used by [`Self::recv_group`].
	index: usize,
	/// Minimum sequence to return from any `recv` method. Set by [`Self::start_at`].
	min_sequence: u64,
	/// One past the highest sequence returned by [`Self::next_group`].
	/// Used only by that method to skip late arrivals; does not affect [`Self::recv_group`].
	next_sequence: u64,
}

impl std::ops::Deref for TrackConsumer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl TrackConsumer {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&kio::Ref<'_, State>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for the next group in arrival order, without blocking.
	///
	/// Returns every group exactly once in the order it landed on the wire — which may be
	/// out of sequence due to network reordering or loss. Use [`Self::poll_next_group`] if
	/// you only want groups whose sequence number is higher than any previously returned.
	///
	/// Returns `Poll::Ready(Ok(Some(group)))` when a group is available,
	/// `Poll::Ready(Ok(None))` when the track is finished,
	/// `Poll::Ready(Err(e))` when the track has been aborted, or
	/// `Poll::Pending` when no group is available yet.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		let Some((consumer, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_group(self.index, self.min_sequence))?)
		else {
			return Poll::Ready(Ok(None));
		};

		self.index = found_index + 1;
		Poll::Ready(Ok(Some(consumer)))
	}

	/// Receive the next group in arrival order.
	///
	/// Every group is returned exactly once, in the order it landed on the wire — which may
	/// be out of sequence due to network reordering or loss. Use [`Self::next_group`] if you
	/// only want groups whose sequence number is higher than any previously returned.
	pub async fn recv_group(&mut self) -> Result<Option<GroupConsumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Poll for the next group with a higher sequence number than any previously returned.
	///
	/// Late arrivals (sequence at or below the last returned) are silently skipped, so this
	/// produces a monotonically increasing sequence at the cost of dropping out-of-order
	/// groups. Use [`Self::poll_recv_group`] to see every group in arrival order instead.
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		loop {
			let Some(group) = ready!(self.poll_recv_group(waiter)?) else {
				return Poll::Ready(Ok(None));
			};
			if group.sequence < self.next_sequence {
				// Late arrival; discard and keep looking.
				continue;
			}
			self.next_sequence = group.sequence.saturating_add(1);
			return Poll::Ready(Ok(Some(group)));
		}
	}

	/// Return the next group with a higher sequence number than any previously returned.
	///
	/// Late arrivals (sequence at or below the last returned) are silently skipped, so this
	/// produces a monotonically increasing sequence at the cost of dropping out-of-order
	/// groups. Use [`Self::recv_group`] to see every group in arrival order instead.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		kio::wait(|waiter| self.poll_next_group(waiter)).await
	}

	/// A helper that calls [`Self::poll_next_group`] and returns its first frame,
	/// skipping the rest of the group. Intended for single-frame groups (see
	/// [`TrackProducer::write_frame`]).
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<bytes::Bytes>>> {
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
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Poll for the group with the given sequence, without blocking.
	pub fn poll_get_group(&self, waiter: &kio::Waiter, sequence: u64) -> Poll<Result<Option<GroupConsumer>>> {
		self.poll(waiter, |state| state.poll_get_group(sequence))
	}

	/// Wait until the group with the given sequence becomes available.
	///
	/// Resolves to `Some(GroupConsumer)` once the group is in the cache.
	/// Resolves to `None` only when `sequence` is at or past the track's
	/// `final_sequence` (set by `finish()` / `finish_at()`), since such a
	/// group can never be produced. Sequences below `final_sequence` still
	/// wait, since older groups may still arrive out of order.
	pub async fn get_group(&self, sequence: u64) -> Result<Option<GroupConsumer>> {
		kio::wait(|waiter| self.poll_get_group(waiter, sequence)).await
	}

	/// Poll for track closure, without blocking.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_closed())
	}

	/// Block until the track is closed.
	///
	/// Returns Ok() is the track was cleanly finished.
	pub async fn closed(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Whether `other` was cloned from this consumer (shares the same underlying state).
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Poll for the total number of groups in the track.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		self.poll(waiter, |state| state.poll_finished())
	}

	/// Block until the track is finished, returning the total number of groups.
	pub async fn finished(&mut self) -> Result<u64> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Start the consumer at the specified sequence.
	pub fn start_at(&mut self, sequence: u64) {
		self.min_sequence = sequence;
	}

	/// Return the latest sequence number in the track.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().max_sequence
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			info: self.info.clone(),
			state: self.state.weak(),
		}
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
	use std::time::Duration;

	/// Helper: count non-tombstoned groups in state.
	fn live_groups(state: &State) -> usize {
		state.groups.iter().flatten().count()
	}

	/// Helper: get the sequence number of the first live group.
	fn first_live_sequence(state: &State) -> u64 {
		state.groups.iter().flatten().next().unwrap().group.sequence
	}

	/// A cache large enough to retain many small groups, with no age bound.
	fn unbounded_cache() -> Cache {
		Cache::new(cache::Config::default().with_max_bytes(u64::MAX))
	}

	#[tokio::test]
	async fn default_keeps_only_latest_group() {
		let mut producer = Track::new("test").produce();

		// Without a cache, each appended group supersedes the previous one immediately.
		producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1
		producer.append_group().unwrap(); // seq 2

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 1, "only the latest group is retained");
		assert_eq!(first_live_sequence(&state), 2);
		assert!(!state.duplicates.contains(&0));
		assert!(!state.duplicates.contains(&1));
		assert!(state.duplicates.contains(&2));
	}

	#[tokio::test]
	async fn cache_retains_history_up_to_bytes() {
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());

		// With a cache, superseded groups stay retained.
		for _ in 0..4 {
			let mut g = producer.append_group().unwrap();
			g.write_frame(bytes::Bytes::from_static(b"x")).unwrap();
		}

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 4, "cache retains every group within budget");
	}

	#[tokio::test]
	async fn cache_bytes_evicts_oldest() {
		let mut producer = Track::new("test")
			.produce()
			.with_cache(Cache::new(cache::Config::default().with_max_bytes(20)));

		// Each non-latest group costs 10 bytes; the 20-byte budget holds two of them plus the
		// (uncounted) latest group.
		for _ in 0..4 {
			let mut g = producer.append_group().unwrap();
			g.write_frame(bytes::Bytes::from(vec![0u8; 10])).unwrap();
		}

		let state = producer.state.read();
		// seq 3 is the latest (not in the cache); seq 1 and 2 fit the budget; seq 0 is evicted.
		assert!(
			!state.duplicates.contains(&0),
			"oldest group evicted under byte pressure"
		);
		assert!(state.duplicates.contains(&1));
		assert!(state.duplicates.contains(&2));
		assert!(state.duplicates.contains(&3));
	}

	#[tokio::test]
	async fn cache_age_evicts_by_wall_clock() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce().with_cache(Cache::new(
			cache::Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(5)),
		));

		producer.append_group().unwrap(); // seq 0 (will be cached once superseded)
		producer.append_group().unwrap(); // seq 1, supersedes seq 0

		// seq 0 is cached and fresh: still retained.
		assert_eq!(live_groups(&producer.state.read()), 2);

		// Advance past max_age, then append to trigger age eviction.
		tokio::time::advance(Duration::from_secs(6)).await;
		producer.append_group().unwrap(); // seq 2

		let state = producer.state.read();
		// seq 0 aged out; seq 1 was just superseded (fresh access at its own insert time) and
		// kept; seq 2 is the latest.
		assert!(!state.duplicates.contains(&0), "group older than max_age is evicted");
		assert!(state.duplicates.contains(&2));
	}

	#[tokio::test]
	async fn cache_access_keeps_group_alive() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce().with_cache(Cache::new(
			cache::Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(5)),
		));

		producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1, supersedes seq 0 (now cached)

		let consumer = producer.consume();

		// Keep accessing seq 0 so its last-access stays recent.
		for _ in 0..4 {
			tokio::time::advance(Duration::from_secs(2)).await;
			assert!(consumer.get_group(0).now_or_never().unwrap().unwrap().is_some());
			producer.append_group().unwrap(); // bump max_sequence + run eviction
		}

		// Despite total elapsed time well past max_age, seq 0 survived because it was accessed
		// within every window.
		assert!(
			producer.state.read().duplicates.contains(&0),
			"a recently accessed group is not aged out"
		);
	}

	#[tokio::test]
	async fn shared_cache_one_budget_across_tracks() {
		let cache = Cache::new(cache::Config::default().with_max_bytes(20));

		let mut track_a = Track::new("a").produce().with_cache(cache.clone());
		let mut track_b = Track::new("b").produce().with_cache(cache.clone());

		// Fill track A with two 10-byte non-latest groups (seq 0, 1) under the latest (seq 2).
		// That alone is exactly the 20-byte budget across A.
		let a0 = {
			let mut g = track_a.append_group().unwrap(); // seq 0
			g.write_frame(bytes::Bytes::from(vec![0u8; 10])).unwrap();
			g
		};
		for _ in 0..2 {
			let mut g = track_a.append_group().unwrap(); // seq 1, then 2
			g.write_frame(bytes::Bytes::from(vec![0u8; 10])).unwrap();
		}
		assert!(!a0.is_aborted(), "A's oldest fits the budget so far");

		// Producing on track B draws from the same 20-byte budget, evicting A's oldest first.
		for _ in 0..2 {
			let mut g = track_b.append_group().unwrap();
			g.write_frame(bytes::Bytes::from(vec![0u8; 10])).unwrap();
		}

		// A's oldest group was aborted by the shared cache to make room for B's cached group.
		assert!(a0.is_aborted(), "shared budget evicts across tracks");
	}

	#[tokio::test]
	async fn cache_never_evicts_max_sequence() {
		// A budget of 0 still keeps the latest group: it is never handed to the cache.
		let mut producer = Track::new("test")
			.produce()
			.with_cache(Cache::new(cache::Config::default().with_max_bytes(0)));

		let mut g = producer.append_group().unwrap();
		g.write_frame(bytes::Bytes::from(vec![0u8; 1024])).unwrap();

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 1, "the latest group survives a zero budget");
		assert_eq!(first_live_sequence(&state), 0);
	}

	#[tokio::test]
	async fn consumer_skips_evicted_groups() {
		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap(); // seq 0

		let mut consumer = producer.consume();

		producer.append_group().unwrap(); // seq 1, supersedes (and drops) seq 0

		// Group 0 was evicted (latest-only). Consumer should get group 1.
		let group = consumer.assert_group();
		assert_eq!(group.sequence, 1);
	}

	#[tokio::test]
	async fn out_of_order_max_sequence_at_front() {
		let mut producer = Track::new("test").produce();

		// Arrive out of order: seq 5 first, then 3, then 4.
		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 4 }).unwrap();

		// max_sequence stays 5; without a cache every non-latest arrival is dropped at once.
		{
			let state = producer.state.read();
			assert_eq!(state.max_sequence, Some(5));
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 5);
		}

		// Append seq 6 (becomes new max_sequence); seq 5 is now superseded and dropped.
		producer.append_group().unwrap(); // seq 6

		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 6);
			assert!(!state.duplicates.contains(&5));
			assert!(state.duplicates.contains(&6));
		}
	}

	#[tokio::test]
	async fn max_sequence_at_front_blocks_trim() {
		// With a cache, an out-of-order late arrival behind the protected max_sequence is
		// retained and a consumer can read through to it.
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());

		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 2 }).unwrap();

		{
			let state = producer.state.read();
			// seq 5 is the protected latest; 3 and 2 are cached behind it.
			assert_eq!(live_groups(&state), 3);
			assert_eq!(state.offset, 0);
			assert!(state.duplicates.contains(&5));
			assert!(state.duplicates.contains(&3));
			assert!(state.duplicates.contains(&2));
		}

		// consume() starts at index 0, first group in arrival order is seq 5.
		let mut consumer = producer.consume();
		assert_eq!(consumer.assert_group().sequence, 5);
	}

	#[tokio::test]
	async fn abort_clears_cached_groups() {
		// A cache retains both groups so we can verify abort drops them and releases the budget.
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());
		producer.append_group().unwrap();
		producer.append_group().unwrap();

		// A stale consumer that never drains must not pin the cached groups.
		let mut consumer = producer.consume();
		assert_eq!(live_groups(&producer.state.read()), 2);

		producer.abort(Error::Cancel).unwrap();

		{
			let state = producer.state.read();
			assert!(state.groups.is_empty(), "cached groups should be dropped on abort");
			assert!(state.duplicates.is_empty());
		}

		// The consumer now surfaces the abort error rather than the leftover cache.
		let result = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(result, Err(Error::Cancel)));
	}

	#[tokio::test]
	async fn drop_unfinished_clears_cached_groups() {
		let producer = Track::new("test").produce();
		let mut writer = producer.clone();
		writer.append_group().unwrap();

		// A stale consumer keeps the channel (and thus the cache) alive.
		let mut consumer = producer.consume();
		assert_eq!(live_groups(&producer.state.read()), 1);

		// Drop every producer without finishing: the cache is released.
		drop(writer);
		drop(producer);

		let result = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn drop_finished_keeps_cached_groups() {
		let mut producer = Track::new("test").produce();
		producer.append_group().unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		drop(producer);

		// A cleanly finished track keeps its cache so the consumer can still drain.
		assert_eq!(consumer.assert_group().sequence, 0);
		let done = consumer.recv_group().now_or_never().expect("should not block").unwrap();
		assert!(done.is_none(), "consumer should drain then see clean finish");
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
		assert_eq!(consumer.assert_group().sequence, 1);

		let done = consumer
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track should finish without waiting for gaps");
	}

	#[tokio::test]
	async fn next_group_skips_late_arrivals() {
		let mut producer = Track::new("test").produce();
		let mut consumer = producer.consume();

		// Seq 5 arrives first.
		producer.create_group(Group { sequence: 5 }).unwrap();
		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);

		// Seq 3 arrives late — skipped because 3 <= 5.
		producer.create_group(Group { sequence: 3 }).unwrap();
		// Seq 4 arrives late — also skipped.
		producer.create_group(Group { sequence: 4 }).unwrap();
		// Seq 7 arrives — returned.
		producer.create_group(Group { sequence: 7 }).unwrap();

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 7);

		// No more groups — would block.
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"should block waiting for a higher sequence"
		);
	}

	#[tokio::test]
	async fn next_group_returns_arrivals_in_order() {
		// A cache keeps both groups retained so the consumer sees the full arrival order.
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());
		let mut consumer = producer.consume();

		// Seq 3 arrives first, then seq 5 — both should be returned in arrival order.
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 5 }).unwrap();

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 3);

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);
	}

	#[tokio::test]
	async fn recv_group_after_next_group_sees_late_arrivals() {
		// A cache retains the late seq 3 behind the protected latest seq 5.
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());
		let mut consumer = producer.consume();

		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();

		// Ordered returns seq 5 and advances its internal cursor past it.
		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);

		// Intermixing: recv_group on the same consumer still returns the late seq 3.
		// The ordered cursor is separate from the recv_group filter.
		assert_eq!(consumer.assert_group().sequence, 3);
	}

	#[tokio::test]
	async fn read_frame_returns_single_frame_per_group() {
		// A cache retains both single-frame groups so the consumer can read each in turn.
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());
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
		// A cache retains group 0 so the consumer reads its first frame before group 1.
		let mut producer = Track::new("test").produce().with_cache(unbounded_cache());
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
}
