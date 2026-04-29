//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackConsumer] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequence number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [TrackProducer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackConsumer] is a fanout handle: it doesn't iterate groups itself but
//! can be cloned, queried for cached groups by sequence, and produce a
//! [TrackSubscriber] via [`TrackConsumer::subscribe`]. The subscriber is what
//! delivers groups, respects the per-subscriber [`Subscription`] preferences,
//! and feeds those preferences into the producer's aggregate so the publisher
//! can react to subscription state (priority, latency, group range).
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

/// Subscription preferences for a single subscriber.
///
/// Describes how groups should be delivered: priority, ordering, latency
/// bounds, and the range of groups requested. The producer aggregates the
/// preferences across all live subscribers so the publisher can serve the
/// union (highest priority, lowest start, etc.).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subscription {
	pub priority: u8,
	pub ordered: bool,
	/// Maximum cache/latency. `Duration::ZERO` means unlimited.
	pub max_latency: Duration,
	/// First group sequence to deliver. `None` is treated as 0 — i.e. deliver
	/// all cached history plus future groups. For "live from now" semantics,
	/// pass `Some(latest + 1)` (or `Some(latest)` to include the in-flight
	/// group). `Subscription::default()` therefore yields full history.
	pub start_group: Option<u64>,
	/// Last group sequence to deliver. `None` means no end (live).
	pub end_group: Option<u64>,
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

	/// Pending fetch requests waiting for a [`TrackDynamic`] to fulfill them.
	/// Each entry is a freshly-minted [`GroupProducer`] for the requested
	/// sequence; the dynamic handler pops it and fills frames.
	fetch_requests: VecDeque<GroupProducer>,

	/// Number of live [`TrackDynamic`] instances. When 0, fetch requests for
	/// uncached groups fail with [`Error::NotFound`].
	dynamic_groups: usize,
}

impl State {
	/// Find the next non-tombstoned group at or after `index` in arrival order.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	fn poll_recv_group(&self, index: usize, min_sequence: u64) -> Poll<Result<Option<(GroupConsumer, usize)>>> {
		let start = index.saturating_sub(self.offset);
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			if let Some((group, _)) = slot
				&& group.sequence >= min_sequence
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
			if group.sequence < next_sequence {
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

			if Some(group.sequence) == self.max_sequence {
				continue;
			}

			if now.duration_since(*created_at) <= MAX_GROUP_AGE {
				break;
			}

			self.duplicates.remove(&group.sequence);
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

	/// "Ready" means at least one group exists, the track is finished, or it's aborted.
	fn poll_ready(&self) -> Poll<Result<()>> {
		if self.max_sequence.is_some() || self.final_sequence.is_some() {
			Poll::Ready(Ok(()))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Aggregate the active (non-closed) subscriber preferences into a single
	/// [`Subscription`].
	///
	/// Aggregation rules:
	/// - `priority`: max across all subscribers (highest-priority wins).
	/// - `ordered`: AND of all subscribers (true only if every subscriber requests ordered).
	/// - `max_latency`: max across subscribers, with `Duration::ZERO` meaning unlimited (ZERO wins).
	/// - `start`: `None` is the maximally-permissive bound (deliver from the
	///   beginning); any subscriber with `None` yields `None`. Otherwise pick
	///   the minimum numeric start.
	/// - `end`: `None` is the maximally-permissive bound (no end); any
	///   subscriber with `None` yields `None`. Otherwise pick the maximum
	///   numeric end.
	///
	/// Returns `None` if there are no active subscriptions.
	fn subscription(&self) -> Option<Subscription> {
		if self.subscriptions.is_empty() {
			return None;
		}

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
		let ordered = subs.iter().all(|s| s.ordered);

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

		let start_group = subs
			.iter()
			.map(|s| s.start_group)
			.reduce(|a, b| match (a, b) {
				(Some(a), Some(b)) => Some(a.min(b)),
				_ => None,
			})
			.unwrap();

		let end_group = subs
			.iter()
			.map(|s| s.end_group)
			.reduce(|a, b| match (a, b) {
				(Some(a), Some(b)) => Some(a.max(b)),
				_ => None,
			})
			.unwrap();

		Some(Subscription {
			priority,
			ordered,
			max_latency,
			start_group,
			end_group,
		})
	}
}

/// A producer for a track, used to create new groups.
pub struct TrackProducer {
	info: Track,
	state: conducer::Producer<State>,
	/// The last aggregate subscription returned by [`Self::poll_subscription`].
	prev_subscription: Option<Subscription>,
}

impl std::ops::Deref for TrackProducer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
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

	/// Create a new consumer for the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
		}
	}

	/// Opt in to handling FETCH requests for groups not in the cache.
	///
	/// While at least one [`TrackDynamic`] is held, [`TrackConsumer::get_group`]
	/// can route uncached requests to the publisher. When all dynamics are
	/// dropped, pending requests are aborted and future uncached fetches return
	/// [`Error::NotFound`].
	pub fn dynamic(&self) -> TrackDynamic {
		TrackDynamic::new(self.info.clone(), self.state.clone())
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

	/// Poll for changes to the aggregate subscription.
	///
	/// Returns `Ready(Some(sub))` when the aggregate differs from the last value
	/// returned. Returns `Ready(None)` when no subscriptions are active (or the
	/// track is closed).
	pub fn poll_subscription(&mut self, waiter: &conducer::Waiter) -> Poll<Option<Subscription>> {
		let prev = self.prev_subscription.clone();
		match self.state.poll(waiter, |state| {
			// Drop closed subscription consumers.
			state.subscriptions.retain(|c| !c.read().is_closed());

			// Register the waiter on each per-subscriber channel so we wake when any
			// individual subscription changes (e.g. update()).
			for sub in &state.subscriptions {
				let _ = sub.poll(waiter, |_| Poll::<()>::Pending);
			}

			let current = state.subscription();
			if current != prev {
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

impl From<Track> for TrackProducer {
	fn from(info: Track) -> Self {
		TrackProducer::new(info)
	}
}

/// Handles on-demand group fetches for a track.
///
/// Created via [`TrackProducer::dynamic`]. While alive, [`TrackConsumer::get_group`]
/// requests for uncached groups are queued for the dynamic handler to fulfill
/// via [`Self::requested_group`]. Dropping the last dynamic aborts any pending
/// requests with [`Error::Cancel`].
pub struct TrackDynamic {
	info: Track,
	state: conducer::Producer<State>,
}

impl Clone for TrackDynamic {
	fn clone(&self) -> Self {
		Self::new(self.info.clone(), self.state.clone())
	}
}

impl std::ops::Deref for TrackDynamic {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl TrackDynamic {
	fn new(info: Track, state: conducer::Producer<State>) -> Self {
		if let Ok(mut state) = state.write() {
			state.dynamic_groups += 1;
		}
		Self { info, state }
	}

	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: FnMut(&mut conducer::Mut<'_, State>) -> Poll<R>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(r) => Ok(r),
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for the next pending fetch request.
	///
	/// Yields a [`GroupProducer`] the publisher fills in. The producer's
	/// `sequence` is the requested group number.
	pub fn poll_requested_group(&mut self, waiter: &conducer::Waiter) -> Poll<Result<GroupProducer>> {
		self.poll(waiter, |state| match state.fetch_requests.pop_front() {
			Some(producer) => Poll::Ready(producer),
			None => Poll::Pending,
		})
	}

	/// Block until a consumer requests a group, returning its producer.
	pub async fn requested_group(&mut self) -> Result<GroupProducer> {
		conducer::wait(|waiter| self.poll_requested_group(waiter)).await
	}

	/// Return true if this is the same dynamic instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for TrackDynamic {
	fn drop(&mut self) {
		if let Ok(mut state) = self.state.write() {
			state.dynamic_groups = state.dynamic_groups.saturating_sub(1);
			if state.dynamic_groups != 0 {
				return;
			}

			// No remaining handler; cancel any pending requests.
			for mut request in state.fetch_requests.drain(..) {
				request.abort(Error::Cancel).ok();
			}
		}
	}
}

#[cfg(test)]
impl TrackDynamic {
	pub fn assert_request(&mut self) -> GroupProducer {
		use futures::FutureExt;
		self.requested_group()
			.now_or_never()
			.expect("should not have blocked")
			.expect("should not have errored")
	}

	pub fn assert_no_request(&mut self) {
		use futures::FutureExt;
		assert!(self.requested_group().now_or_never().is_none(), "should have blocked");
	}
}

/// A weak reference to a track that doesn't prevent auto-close.
#[derive(Clone)]
pub(crate) struct TrackWeak {
	pub(crate) info: Track,
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

/// A consumer for a track.
///
/// `TrackConsumer` is a fanout handle: clone it freely, query for cached
/// groups via [`Self::get_group`], or call [`Self::subscribe`] to get a
/// [`TrackSubscriber`] that delivers groups respecting [`Subscription`]
/// preferences.
#[derive(Clone)]
pub struct TrackConsumer {
	info: Track,
	state: conducer::Consumer<State>,
}

impl std::ops::Deref for TrackConsumer {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl TrackConsumer {
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

	/// Register a subscription and return a [`TrackSubscriber`].
	///
	/// The subscriber can be used right away; callers may want to call
	/// [`TrackSubscriber::ready`] to wait until the first group exists (or
	/// finish/abort). Dropping the subscriber removes it from the producer's
	/// aggregate.
	pub fn subscribe(&self, sub: Subscription) -> Result<TrackSubscriber> {
		let sub_producer = conducer::Producer::new(sub);
		let sub_consumer = sub_producer.consume();

		// Insert via a weak handle so we don't bump the producer ref count and prevent auto-close.
		// If the channel is already closed, skip — the subscriber can still drain cached groups.
		let weak = self.state.weak();
		if let Ok(mut state) = weak.write() {
			state.subscriptions.push(sub_consumer);
		}

		Ok(TrackSubscriber {
			info: self.info.clone(),
			state: self.state.clone(),
			sub: sub_producer,
			index: 0,
			next: 0,
		})
	}

	/// Convenience: [`Self::subscribe`] with [`Subscription::default`].
	pub fn subscribe_default(&self) -> Result<TrackSubscriber> {
		self.subscribe(Subscription::default())
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
	pub fn poll_finished(&self, waiter: &conducer::Waiter) -> Poll<Result<u64>> {
		self.poll(waiter, |state| state.poll_finished())
	}

	/// Block until the track is finished, returning the total number of groups.
	pub async fn finished(&self) -> Result<u64> {
		conducer::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Return the latest cached group, if any.
	pub fn latest_group(&self) -> Option<GroupConsumer> {
		let state = self.state.read();
		let max = state.max_sequence?;
		state
			.groups
			.iter()
			.flatten()
			.find_map(|(g, _)| (g.sequence == max).then(|| g.consume()))
	}

	/// Get a specific group.
	///
	/// - Cache hit → returns the cached consumer (a [`TrackDynamic`] handler is not required).
	/// - Cache miss + no [`TrackDynamic`] handler → returns [`Error::NotFound`].
	/// - Cache miss + handler present → routes a request to the producer; returns
	///   a [`GroupConsumer`] that fills in as the publisher writes frames. Errors
	///   later if the publisher aborts/declines (e.g. group is past final).
	pub fn get_group(&self, info: Group) -> Result<GroupConsumer> {
		let sequence = info.sequence;

		// Upgrade to a temporary producer so we can mutate the state.
		let producer = self
			.state
			.produce()
			.ok_or_else(|| self.state.read().abort.clone().unwrap_or(Error::Dropped))?;

		let mut state = producer
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))?;

		// Cache hit: always return, regardless of dynamic handler state.
		for (group, _) in state.groups.iter().flatten() {
			if group.sequence == sequence {
				return Ok(group.consume());
			}
		}

		// Track is finalized: groups at/after the final boundary can never exist,
		// so fail fast instead of queueing an impossible request.
		if let Some(fin) = state.final_sequence
			&& sequence >= fin
		{
			return Err(Error::NotFound);
		}

		if state.dynamic_groups == 0 {
			return Err(Error::NotFound);
		}

		// Queue a request for the dynamic handler; cache the producer so
		// concurrent fetches for the same sequence share frames.
		let group = info.produce();
		let consumer = group.consume();
		let now = tokio::time::Instant::now();
		state.duplicates.insert(sequence);
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(sequence));
		state.groups.push_back(Some((group.clone(), now)));
		state.fetch_requests.push_back(group);
		state.evict_expired(now);

		Ok(consumer)
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
			prev_subscription: None,
		})
	}
}

/// Iterates groups from a track while managing this subscriber's lifecycle.
///
/// Created via [`TrackConsumer::subscribe`]. Registers a [`Subscription`] in
/// the shared state on creation; automatically removes it on drop. The
/// producer's aggregate reflects this subscriber's preferences as long as the
/// subscriber is alive.
pub struct TrackSubscriber {
	info: Track,
	state: conducer::Consumer<State>,
	sub: conducer::Producer<Subscription>,
	/// Arrival-order cursor used by [`Self::recv_group`] / [`Self::next_group`].
	index: usize,
	/// One past the highest sequence returned by [`Self::next_group`] /
	/// [`Self::read_frame`]. Late arrivals below this are silently skipped by
	/// those methods (but [`Self::recv_group`] still returns them).
	next: u64,
}

impl std::ops::Deref for TrackSubscriber {
	type Target = Track;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl TrackSubscriber {
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

	/// Poll whether the track is "ready": has at least one group, is finished, or aborted.
	pub fn poll_ready(&self, waiter: &conducer::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_ready())
	}

	/// Wait until the track is ready (has at least one group, is finished, or aborted).
	pub async fn ready(&self) -> Result<()> {
		conducer::wait(|waiter| self.poll_ready(waiter)).await
	}

	/// Poll for the next group received over the network, in arrival order.
	///
	/// Respects this subscriber's [`Subscription::start_group`] / `end_group` bounds.
	/// Groups may arrive out of order or with gaps. Use [`Self::next_group`]
	/// if you need monotonic delivery (skipping late arrivals).
	///
	/// Returns `Ok(None)` once the track is finished or the first group past
	/// `end` is observed; the subscription is considered complete and further
	/// polls will keep returning `Ok(None)`.
	pub fn poll_recv_group(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		let sub = self.sub.read();
		let min_sequence = sub.start_group.unwrap_or(0);
		let end = sub.end_group;
		drop(sub);

		let Some((consumer, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_group(self.index, min_sequence))?)
		else {
			return Poll::Ready(Ok(None));
		};

		if let Some(end) = end
			&& consumer.sequence > end
		{
			return Poll::Ready(Ok(None));
		}

		self.index = found_index + 1;
		Poll::Ready(Ok(Some(consumer)))
	}

	/// Receive the next group available on this track, in arrival order.
	pub async fn recv_group(&mut self) -> Result<Option<GroupConsumer>> {
		conducer::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Return the next group with a strictly-greater sequence number than the last returned.
	///
	/// Groups that arrive late (sequence at or below the last one returned) are
	/// silently skipped. Respects [`Subscription::start_group`] / `end_group`.
	pub fn poll_next_group(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		let sub = self.sub.read();
		let min_sequence = self.next.max(sub.start_group.unwrap_or(0));
		let end = sub.end_group;
		drop(sub);

		let Some((consumer, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_group(self.index, min_sequence))?)
		else {
			return Poll::Ready(Ok(None));
		};

		if let Some(end) = end
			&& consumer.sequence > end
		{
			return Poll::Ready(Ok(None));
		}

		self.index = found_index + 1;
		self.next = consumer.sequence.saturating_add(1);
		Poll::Ready(Ok(Some(consumer)))
	}

	/// Block until the next strictly-greater-sequence group is available.
	pub async fn next_group(&mut self) -> Result<Option<GroupConsumer>> {
		conducer::wait(|waiter| self.poll_next_group(waiter)).await
	}

	/// Deprecated alias for [`Self::poll_next_group`].
	#[deprecated(note = "use poll_next_group")]
	pub fn poll_next_group_ordered(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		self.poll_next_group(waiter)
	}

	/// Deprecated alias for [`Self::next_group`].
	#[deprecated(note = "use next_group")]
	pub async fn next_group_ordered(&mut self) -> Result<Option<GroupConsumer>> {
		self.next_group().await
	}

	/// Return the first frame of the next strictly-greater-sequence group,
	/// skipping the rest of the group. Intended for single-frame groups (see
	/// [`TrackProducer::write_frame`]).
	///
	/// Respects [`Subscription::start_group`] / `end_group`.
	pub fn poll_read_frame(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<bytes::Bytes>>> {
		let sub = self.sub.read();
		let lower = self.next.max(sub.start_group.unwrap_or(0));
		let end = sub.end_group;
		drop(sub);

		let Some((frame, found_index, sequence)) =
			ready!(self.poll(waiter, |state| { state.poll_read_frame(self.index, lower, waiter) })?)
		else {
			return Poll::Ready(Ok(None));
		};

		if let Some(end) = end
			&& sequence > end
		{
			return Poll::Ready(Ok(None));
		}

		self.index = found_index + 1;
		self.next = sequence.saturating_add(1);
		Poll::Ready(Ok(Some(frame)))
	}

	/// Block until a frame is available from the next group in sequence order.
	pub async fn read_frame(&mut self) -> Result<Option<bytes::Bytes>> {
		conducer::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Update this subscription's preferences. Wakes the producer's
	/// [`TrackProducer::poll_subscription`] if the aggregate changed.
	pub fn update(&mut self, sub: Subscription) {
		if let Ok(mut guard) = self.sub.write() {
			*guard = sub;
		}
	}

	/// Read this subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.sub.read().clone()
	}

	/// Return the latest cached group, if any.
	pub fn latest_group(&self) -> Option<GroupConsumer> {
		let state = self.state.read();
		let max = state.max_sequence?;
		state
			.groups
			.iter()
			.flatten()
			.find_map(|(g, _)| (g.sequence == max).then(|| g.consume()))
	}

	/// Poll for track closure, without blocking.
	pub fn poll_closed(&self, waiter: &conducer::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_closed())
	}

	/// Block until the track is closed.
	pub async fn closed(&self) -> Result<()> {
		conducer::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll for the total number of groups in the track.
	pub fn poll_finished(&self, waiter: &conducer::Waiter) -> Poll<Result<u64>> {
		self.poll(waiter, |state| state.poll_finished())
	}

	/// Block until the track is finished.
	pub async fn finished(&self) -> Result<u64> {
		conducer::wait(|waiter| self.poll_finished(waiter)).await
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

impl Drop for TrackSubscriber {
	fn drop(&mut self) {
		// Closing the per-subscriber channel marks it as closed so the producer's
		// aggregator drops it on the next poll.
		let _ = self.sub.close();
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl TrackSubscriber {
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
impl TrackConsumer {
	pub fn assert_subscribe(&self) -> TrackSubscriber {
		self.subscribe_default().expect("subscribe should not have errored")
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
		state.groups.iter().flatten().next().unwrap().0.sequence
	}

	#[tokio::test]
	async fn evict_expired_groups() {
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

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		producer.append_group().unwrap(); // seq 3

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
		producer.append_group().unwrap();

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		producer.append_group().unwrap();

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
		producer.append_group().unwrap();
		producer.append_group().unwrap();
		producer.append_group().unwrap();

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
		let mut subscriber = consumer.assert_subscribe();

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;
		producer.append_group().unwrap(); // seq 1

		// Group 0 was evicted. Subscriber should get group 1.
		let group = subscriber.assert_group();
		assert_eq!(group.sequence, 1);
	}

	#[tokio::test]
	async fn out_of_order_max_sequence_at_front() {
		tokio::time::pause();

		let mut producer = Track::new("test").produce();

		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 4 }).unwrap();

		{
			let state = producer.state.read();
			assert_eq!(state.max_sequence, Some(5));
		}

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		producer.append_group().unwrap(); // seq 6

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

		producer.create_group(Group { sequence: 5 }).unwrap();

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		producer.create_group(Group { sequence: 3 }).unwrap();

		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
		}

		tokio::time::advance(MAX_GROUP_AGE + Duration::from_secs(1)).await;

		producer.create_group(Group { sequence: 2 }).unwrap();

		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
			assert!(state.duplicates.contains(&5));
			assert!(!state.duplicates.contains(&3));
			assert!(state.duplicates.contains(&2));
		}

		let mut subscriber = producer.consume().assert_subscribe();
		let group = subscriber.assert_group();
		// First non-tombstoned group is seq 5.
		assert_eq!(group.sequence, 5);
	}

	#[test]
	fn append_finish_cannot_be_rewritten() {
		let mut producer = Track::new("test").produce();

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

		let mut subscriber = producer.consume().assert_subscribe();
		assert_eq!(subscriber.assert_group().sequence, 1);

		let done = subscriber
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track should finish without waiting for gaps");
	}

	#[tokio::test]
	async fn next_group_skips_late_arrivals() {
		let mut producer = Track::new("test").produce();
		let mut subscriber = producer.consume().assert_subscribe();

		producer.create_group(Group { sequence: 5 }).unwrap();
		let group = subscriber
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);

		// Late arrivals are skipped.
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 4 }).unwrap();
		producer.create_group(Group { sequence: 7 }).unwrap();

		let group = subscriber
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 7);

		assert!(
			subscriber.next_group().now_or_never().is_none(),
			"should block waiting for a higher sequence"
		);
	}

	#[tokio::test]
	async fn next_group_returns_arrivals_in_order() {
		let mut producer = Track::new("test").produce();
		let mut subscriber = producer.consume().assert_subscribe();

		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 5 }).unwrap();

		let group = subscriber
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 3);

		let group = subscriber
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);
	}

	#[tokio::test]
	async fn recv_group_after_next_group_sees_late_arrivals() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();
		let mut ordered = consumer.assert_subscribe();
		let mut arrival = consumer.assert_subscribe();

		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();

		// Ordered subscriber sees seq 5 and skips the late seq 3.
		let group = ordered
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);
		assert!(
			ordered.next_group().now_or_never().is_none(),
			"ordered should block waiting for >5"
		);

		// Arrival-order subscriber sees both, in arrival order.
		assert_eq!(arrival.assert_group().sequence, 5);
		assert_eq!(arrival.assert_group().sequence, 3);
	}

	#[tokio::test]
	async fn read_frame_returns_single_frame_per_group() {
		let mut producer = Track::new("test").produce();
		let mut subscriber = producer.consume().assert_subscribe();

		producer.write_frame(b"hello".as_slice()).unwrap();
		producer.write_frame(b"world".as_slice()).unwrap();

		let frame = subscriber
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"hello");

		let frame = subscriber
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
		let mut subscriber = producer.consume().assert_subscribe();

		let _stalled = producer.create_group(Group { sequence: 3 }).unwrap();
		let mut g5 = producer.create_group(Group { sequence: 5 }).unwrap();
		g5.write_frame(bytes::Bytes::from_static(b"later")).unwrap();
		g5.finish().unwrap();

		let frame = subscriber
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
		let mut subscriber = producer.consume().assert_subscribe();

		let mut g0 = producer.create_group(Group { sequence: 0 }).unwrap();
		g0.write_frame(bytes::Bytes::from_static(b"one")).unwrap();
		g0.write_frame(bytes::Bytes::from_static(b"two")).unwrap();
		g0.finish().unwrap();

		producer.write_frame(b"next".as_slice()).unwrap();

		let frame = subscriber
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"one");

		let frame = subscriber
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"next");
	}

	#[tokio::test]
	async fn read_frame_waits_for_pending_group_after_finish() {
		let mut producer = Track::new("test").produce();
		let mut subscriber = producer.consume().assert_subscribe();

		let mut g0 = producer.create_group(Group { sequence: 0 }).unwrap();
		producer.finish().unwrap();

		assert!(
			subscriber.read_frame().now_or_never().is_none(),
			"read_frame must block on a pending group even after finish()"
		);

		g0.write_frame(bytes::Bytes::from_static(b"late")).unwrap();
		let frame = subscriber
			.read_frame()
			.now_or_never()
			.expect("should not block once a frame is written")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"late");
	}

	#[tokio::test]
	async fn read_frame_respects_subscription_start() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();
		let mut subscriber = consumer
			.subscribe(Subscription {
				start_group: Some(5),
				..Default::default()
			})
			.unwrap();

		let mut g3 = producer.create_group(Group { sequence: 3 }).unwrap();
		g3.write_frame(bytes::Bytes::from_static(b"skip-me")).unwrap();
		g3.finish().unwrap();

		let mut g5 = producer.create_group(Group { sequence: 5 }).unwrap();
		g5.write_frame(bytes::Bytes::from_static(b"keep")).unwrap();
		g5.finish().unwrap();

		let frame = subscriber
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
		let mut subscriber = producer.consume().assert_subscribe();

		producer.write_frame(b"only".as_slice()).unwrap();
		producer.finish().unwrap();

		let frame = subscriber
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame[..], b"only");

		let done = subscriber
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none());
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

		let got = consumer.produce().expect("should produce");
		assert!(got.is_clone(&producer), "should be the same track");

		got.clone().append_group().unwrap();
		let mut subscriber = producer.consume().assert_subscribe();
		subscriber.assert_group(); // group 0
		subscriber.assert_group(); // group 1
	}

	#[tokio::test]
	async fn consumer_produce_after_drop() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		drop(producer);

		let err = consumer.produce();
		assert!(matches!(err, Err(Error::Dropped)), "expected Dropped");
	}

	#[tokio::test]
	async fn consumer_produce_after_abort() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();
		producer.abort(Error::Cancel).unwrap();
		drop(producer);

		let err = consumer.produce();
		assert!(matches!(err, Err(Error::Cancel)), "expected Cancel");
	}

	#[tokio::test]
	async fn consumer_produce_keeps_alive() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let upgraded = consumer.produce().expect("should produce");
		drop(producer);

		assert!(consumer.closed().now_or_never().is_none(), "should not be closed");
		drop(upgraded);

		assert!(consumer.closed().now_or_never().is_some(), "should be closed");
	}

	#[tokio::test]
	async fn aggregate_subscription_priority_max() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let _a = consumer
			.subscribe(Subscription {
				priority: 1,
				..Default::default()
			})
			.unwrap();
		let _b = consumer
			.subscribe(Subscription {
				priority: 5,
				..Default::default()
			})
			.unwrap();

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.priority, 5);
		assert!(!agg.ordered);
	}

	#[tokio::test]
	async fn aggregate_subscription_ordered_and() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let _a = consumer
			.subscribe(Subscription {
				ordered: true,
				..Default::default()
			})
			.unwrap();
		let _b = consumer
			.subscribe(Subscription {
				ordered: false,
				..Default::default()
			})
			.unwrap();

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert!(!agg.ordered, "ordered should be AND of all");
	}

	#[tokio::test]
	async fn aggregate_subscription_max_latency_zero_wins() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let _a = consumer
			.subscribe(Subscription {
				max_latency: Duration::from_secs(1),
				..Default::default()
			})
			.unwrap();
		let _b = consumer
			.subscribe(Subscription {
				max_latency: Duration::ZERO,
				..Default::default()
			})
			.unwrap();

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.max_latency, Duration::ZERO, "ZERO (unlimited) should win");
	}

	#[tokio::test]
	async fn aggregate_subscription_start_min_end_max() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let _a = consumer
			.subscribe(Subscription {
				start_group: Some(5),
				end_group: Some(20),
				..Default::default()
			})
			.unwrap();
		let _b = consumer
			.subscribe(Subscription {
				start_group: Some(3),
				end_group: Some(10),
				..Default::default()
			})
			.unwrap();

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.start_group, Some(3));
		assert_eq!(agg.end_group, Some(20));
	}

	#[tokio::test]
	async fn aggregate_subscription_none_wins_over_some() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		// One concrete bound, one unbounded — the unbounded subscriber wants the full range,
		// so the aggregate must be unbounded too.
		let _a = consumer
			.subscribe(Subscription {
				start_group: Some(5),
				end_group: Some(20),
				..Default::default()
			})
			.unwrap();
		let _b = consumer.subscribe(Subscription::default()).unwrap();

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.start_group, None, "None subscriber wants from the beginning");
		assert_eq!(agg.end_group, None, "None subscriber wants no end");
	}

	#[tokio::test]
	async fn aggregate_subscription_drops_on_subscriber_drop() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let a = consumer
			.subscribe(Subscription {
				priority: 1,
				..Default::default()
			})
			.unwrap();

		// First poll establishes the baseline.
		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.priority, 1);

		drop(a);

		// After the only subscriber drops, the aggregate becomes None on next change.
		let agg = producer.subscription().now_or_never().expect("should not block");
		assert!(agg.is_none(), "no live subscribers should yield None");
	}

	#[tokio::test]
	async fn aggregate_subscription_reflects_update() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();

		let mut sub = consumer
			.subscribe(Subscription {
				priority: 1,
				..Default::default()
			})
			.unwrap();

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.priority, 1);

		sub.update(Subscription {
			priority: 7,
			..Default::default()
		});

		let agg = producer
			.subscription()
			.now_or_never()
			.expect("should not block")
			.expect("aggregate should exist");
		assert_eq!(agg.priority, 7);
	}

	#[tokio::test]
	async fn recv_group_respects_end_bound() {
		let mut producer = Track::new("test").produce();
		let consumer = producer.consume();
		let mut subscriber = consumer
			.subscribe(Subscription {
				end_group: Some(2),
				..Default::default()
			})
			.unwrap();

		producer.create_group(Group { sequence: 1 }).unwrap();
		producer.create_group(Group { sequence: 2 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();

		assert_eq!(subscriber.assert_group().sequence, 1);
		assert_eq!(subscriber.assert_group().sequence, 2);
		// Group 3 is past `end`; subscriber should treat the stream as done.
		let done = subscriber
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "groups past end should yield None");
	}

	#[tokio::test]
	async fn get_group_cache_hit() {
		let mut producer = Track::new("test").produce();
		producer.create_group(Group { sequence: 5 }).unwrap();
		let consumer = producer.consume();

		// No TrackDynamic registered, but cache hits still succeed.
		let group = consumer.get_group(Group { sequence: 5 }).expect("cache hit");
		assert_eq!(group.sequence, 5);
	}

	#[tokio::test]
	async fn get_group_no_handler_returns_not_found() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();

		match consumer.get_group(Group { sequence: 0 }) {
			Err(Error::NotFound) => {}
			Err(err) => panic!("expected NotFound, got {err:?}"),
			Ok(_) => panic!("expected NotFound, got Ok"),
		}
	}

	#[tokio::test]
	async fn get_group_past_final_returns_not_found() {
		let mut producer = Track::new("test").produce();
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.finish().unwrap();
		let consumer = producer.consume();
		let _dynamic = producer.dynamic();

		// Cached group below the final boundary still works.
		assert_eq!(consumer.get_group(Group { sequence: 3 }).unwrap().sequence, 3);

		// Sequences at/past the final boundary fail fast even with a handler attached.
		match consumer.get_group(Group { sequence: 4 }) {
			Err(Error::NotFound) => {}
			Err(err) => panic!("expected NotFound, got {err:?}"),
			Ok(_) => panic!("expected NotFound, got Ok"),
		}
	}

	#[tokio::test]
	async fn get_group_via_dynamic_handler() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let mut dynamic = producer.dynamic();

		// Issue a fetch; the handler should pop a producer for the same sequence.
		let mut group_consumer = consumer.get_group(Group { sequence: 7 }).expect("queued");
		assert_eq!(group_consumer.sequence, 7);

		let mut group_producer = dynamic.assert_request();
		assert_eq!(group_producer.sequence, 7);

		// Publisher fills frames; consumer reads them.
		group_producer.write_frame(bytes::Bytes::from_static(b"hello")).unwrap();
		group_producer.finish().unwrap();

		let frame = group_consumer.read_frame().await.unwrap();
		assert_eq!(frame.as_deref(), Some(&b"hello"[..]));
		assert!(group_consumer.read_frame().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn get_group_shares_in_flight() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let mut dynamic = producer.dynamic();

		let mut a = consumer.get_group(Group { sequence: 3 }).expect("queued");
		let mut b = consumer
			.get_group(Group { sequence: 3 })
			.expect("cache hit second time");

		// Only one request should be queued; both consumers share the same group.
		let mut group_producer = dynamic.assert_request();
		dynamic.assert_no_request();

		group_producer
			.write_frame(bytes::Bytes::from_static(b"shared"))
			.unwrap();
		group_producer.finish().unwrap();

		assert_eq!(a.read_frame().await.unwrap().as_deref(), Some(&b"shared"[..]));
		assert_eq!(b.read_frame().await.unwrap().as_deref(), Some(&b"shared"[..]));
	}

	#[tokio::test]
	async fn get_group_aborted_by_publisher() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let mut dynamic = producer.dynamic();

		let mut group_consumer = consumer.get_group(Group { sequence: 2 }).expect("queued");
		let mut group_producer = dynamic.assert_request();

		// Publisher decides the group can't be served.
		group_producer.abort(Error::NotFound).unwrap();

		match group_consumer.read_frame().await {
			Err(Error::NotFound) => {}
			other => panic!("expected NotFound, got {other:?}"),
		}
	}

	#[tokio::test]
	async fn get_group_pending_aborted_when_dynamic_dropped() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let dynamic = producer.dynamic();

		let mut group_consumer = consumer.get_group(Group { sequence: 9 }).expect("queued");

		// Dropping the only TrackDynamic aborts the pending request.
		drop(dynamic);

		match group_consumer.read_frame().await {
			Err(Error::Cancel) => {}
			other => panic!("expected Cancel, got {other:?}"),
		}

		// And subsequent fetches for uncached sequences fail with NotFound.
		assert!(matches!(
			consumer.get_group(Group { sequence: 10 }),
			Err(Error::NotFound)
		));
	}

	#[tokio::test]
	async fn get_group_dynamic_clone_keeps_handler_alive() {
		let producer = Track::new("test").produce();
		let consumer = producer.consume();
		let dynamic = producer.dynamic();
		let mut dynamic_clone = dynamic.clone();

		let mut group_consumer = consumer.get_group(Group { sequence: 9 }).expect("queued");

		// Dropping one handle must NOT abort the pending request: the clone
		// still represents a live handler, so the counter must stay positive.
		drop(dynamic);

		let mut group_producer = dynamic_clone.assert_request();
		assert_eq!(group_producer.sequence, 9);

		group_producer.write_frame(bytes::Bytes::from_static(b"clone")).unwrap();
		group_producer.finish().unwrap();

		assert_eq!(
			group_consumer.read_frame().await.unwrap().as_deref(),
			Some(&b"clone"[..])
		);
	}

	#[tokio::test]
	async fn latest_group_returns_max_sequence_consumer() {
		let mut producer = Track::new("test").produce();
		producer.create_group(Group { sequence: 1 }).unwrap();
		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();

		let consumer = producer.consume();
		let latest = consumer.latest_group().expect("has groups");
		assert_eq!(latest.sequence, 5);
	}

	#[tokio::test]
	async fn latest_group_none_on_empty_track() {
		let producer = Track::new("test").produce();
		assert!(producer.consume().latest_group().is_none());
	}
}
