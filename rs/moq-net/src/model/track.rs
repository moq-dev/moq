//! A track is a collection of semi-reliable and semi-ordered streams, split into a [TrackProducer] and [TrackSubscriber] handle.
//!
//! A [TrackProducer] creates streams with a sequence number and priority.
//! The sequence number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [TrackProducer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [TrackSubscriber] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Transport is to not block on them.
//! Streams will be cached for a potentially limited duration added to the unreliable nature.
//! A [TrackConsumer] is a cheap, cloneable handle; subscribing it multiple times fans the same
//! cached streams out to each independent [TrackSubscriber].
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use crate::{Error, Result, Subscription, Timescale, coding};

use super::{Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	pin::Pin,
	sync::Arc,
	task::{Context, Poll, ready},
	time::Duration,
};

/// Default [`TrackInfo::cache`] age when the publisher doesn't set one.
pub const DEFAULT_CACHE: Duration = Duration::from_secs(5);

/// Publisher-side properties of a track.
///
/// These are fixed by the publisher when the track is created and don't change
/// while the track is alive. A subscriber learns them via
/// [`crate::BroadcastConsumer::track`](crate::BroadcastConsumer::track),
/// which returns the publisher's [`TrackInfo`] once the subscription is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TrackInfo {
	/// Hint that this track's frames are worth compressing (e.g. a JSON catalog).
	/// The publisher honors it by negotiating a codec in SUBSCRIBE_OK; codec-less
	/// peers (older drafts) ignore it and send frames verbatim.
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "std::ops::Not::not"))]
	pub compress: bool,
	/// Units per second for per-frame timestamps on this track.
	///
	/// `None` means the publisher hasn't advertised a timescale; subscribers
	/// receive frames with `timestamp: None`. On Lite05+ a `Some(_)` value is
	/// echoed in SUBSCRIBE_OK and the publisher zigzag-delta encodes
	/// per-frame timestamps at that scale on the wire; rejecting a frame at
	/// the wrong scale prevents silent corruption.
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub timescale: Option<Timescale>,
	/// How long the publisher keeps old groups available before evicting them
	/// (the newest group is always retained). A subscriber's
	/// [`Subscription::stale`] window is clamped to this, since a group can't be
	/// waited for longer than it's kept around. Announced in SUBSCRIBE_OK so
	/// relays re-serve with the same window. Defaults to [`DEFAULT_CACHE`].
	#[cfg_attr(
		feature = "serde",
		serde(
			default = "default_cache",
			skip_serializing_if = "is_default_cache",
			with = "cache_millis"
		)
	)]
	pub cache: Duration,
}

#[cfg(feature = "serde")]
fn default_cache() -> Duration {
	DEFAULT_CACHE
}

#[cfg(feature = "serde")]
fn is_default_cache(cache: &Duration) -> bool {
	*cache == DEFAULT_CACHE
}

/// Serialize [`TrackInfo::cache`] as a bare integer of milliseconds, matching the
/// catalog's other durations (and the wire), rather than serde's `{secs, nanos}`.
#[cfg(feature = "serde")]
mod cache_millis {
	use std::time::Duration;

	pub fn serialize<S: serde::Serializer>(cache: &Duration, s: S) -> Result<S::Ok, S::Error> {
		s.serialize_u64(cache.as_millis() as u64)
	}

	pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
		let ms = <u64 as serde::Deserialize>::deserialize(d)?;
		Ok(Duration::from_millis(ms))
	}
}
impl Default for TrackInfo {
	fn default() -> Self {
		Self {
			compress: false,
			timescale: None,
			cache: DEFAULT_CACHE,
		}
	}
}

impl TrackInfo {
	/// Mark this track's frames as worth compressing, returning `self` for chaining.
	pub fn with_compress(mut self, compress: bool) -> Self {
		self.compress = compress;
		self
	}

	/// Set the per-frame timestamp scale, returning `self` for chaining.
	///
	/// Required for Lite05+ peers to encode per-frame timestamps on the wire.
	pub fn with_timescale(mut self, timescale: Timescale) -> Self {
		self.timescale = Some(timescale);
		self
	}

	/// Set how long old groups stay available before eviction, returning `self` for chaining.
	pub fn with_cache(mut self, cache: Duration) -> Self {
		self.cache = cache;
		self
	}

	/// Clamp a subscriber's stale window to this track's [`Self::cache`]: a
	/// subscriber can't wait for a late group longer than the publisher keeps it.
	/// `Duration::ZERO` (skip immediately) is left untouched by the `min`.
	fn clamp_stale(&self, stale: Duration) -> Duration {
		stale.min(self.cache)
	}
}

#[derive(Default)]
struct TrackState {
	// The info for the track; always Some for TrackSubscriber/TrackProducer.
	info: Option<TrackInfo>,

	// Groups in arrival order. `None` entries are tombstones for evicted groups.
	groups: VecDeque<Option<(GroupProducer, tokio::time::Instant)>>,

	// TODO Do we need this?
	duplicates: HashSet<u64>,

	// We've popped the front of this VecDeque this many times, used to map sequence -> index.
	offset: usize,

	// The highest sequence number successfully appended to the track.
	max_sequence: Option<u64>,

	// The sequence number at which the track was finalized.
	final_sequence: Option<u64>,

	// The error that caused the track to be aborted, if any.
	abort: Option<Error>,

	// Active subscriptions.
	subscriptions: Vec<kio::Consumer<Subscription>>,

	// Specific groups requested via `fetch` that aren't cached yet, FIFO for the
	// producer to serve (see `TrackProducer::requested_fetch`).
	fetches: VecDeque<FetchRequest>,
}

impl TrackState {
	fn poll_info(&self) -> Poll<Result<TrackInfo>> {
		if let Some(info) = &self.info {
			Poll::Ready(Ok(info.clone()))
		} else {
			Poll::Pending
		}
	}

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
		waiter: &kio::Waiter,
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

	/// Find the smallest-sequence cached group satisfying
	/// `next_sequence <= seq <= end_sequence (if set)`. Used by
	/// [`TrackSubscriber::next_group`] so the range can be widened (or unset)
	/// after the fact and previously-skipped cached groups become available
	/// without scanning past them in arrival order.
	///
	/// Returns `Poll::Pending` when no in-range group is currently cached but
	/// future groups could still arrive in range; returns `Ok(None)` only when
	/// the track is finalized and no further in-range group is possible.
	fn poll_next_in_range(&self, next_sequence: u64, end_sequence: Option<u64>) -> Poll<Result<Option<GroupConsumer>>> {
		// If the end cap is already below where we'd resume, no group can
		// ever satisfy this call until the cap rises. Pending (not None) so
		// the consumer is parked rather than told the stream is over.
		if let Some(end) = end_sequence
			&& end < next_sequence
		{
			if let Some(err) = &self.abort {
				return Poll::Ready(Err(err.clone()));
			}
			return Poll::Pending;
		}

		let mut best: Option<&GroupProducer> = None;
		for (group, _) in self.groups.iter().flatten() {
			if group.sequence < next_sequence {
				continue;
			}
			if let Some(end) = end_sequence
				&& group.sequence > end
			{
				continue;
			}
			if best.is_none_or(|b| group.sequence < b.sequence) {
				best = Some(group);
			}
		}

		if let Some(group) = best {
			return Poll::Ready(Ok(Some(group.consume())));
		}

		// No in-range group is cached. Decide whether more could ever arrive.
		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		// `final_sequence` is one past the last possible sequence. If our
		// floor is already at/past it, nothing else can land in range.
		if let Some(fin) = self.final_sequence
			&& next_sequence >= fin
		{
			return Poll::Ready(Ok(None));
		}
		Poll::Pending
	}

	fn poll_get_group(&self, sequence: u64) -> Poll<Result<Option<GroupConsumer>>> {
		// Search for the group with the matching sequence, skipping tombstones.
		for (group, _) in self.groups.iter().flatten() {
			if group.sequence == sequence {
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

	/// Evict groups older than `max_age`, never evicting the max_sequence group.
	///
	/// Groups are in arrival order, so we can stop early when we hit a non-expired,
	/// non-max_sequence group (everything after it arrived even later).
	/// When max_sequence is at the front, we skip past it and tombstone expired groups
	/// behind it.
	fn evict_expired(&mut self, now: tokio::time::Instant, max_age: Duration) {
		for slot in self.groups.iter_mut() {
			let Some((group, created_at)) = slot else { continue };

			if Some(group.sequence) == self.max_sequence {
				continue;
			}

			if now.duration_since(*created_at) <= max_age {
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

	fn modify(producer: &kio::Producer<Self>) -> Result<kio::Mut<'_, Self>> {
		producer.write().map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Insert a fetched group with an explicit timescale, independent of whether the
	/// track has been accepted (info may be absent). Used to serve a one-shot FETCH
	/// without a full subscription. The group lands in the cache so a waiting
	/// [`TrackFetchPending`] resolves via [`Self::poll_get_group`].
	fn insert_fetch_group(&mut self, sequence: u64, timescale: Option<Timescale>) -> Result<GroupProducer> {
		if let Some(err) = &self.abort {
			return Err(err.clone());
		}
		if let Some(fin) = self.final_sequence
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}
		if !self.duplicates.insert(sequence) {
			return Err(Error::Duplicate);
		}

		let group = GroupProducer::new_with_timescale(Group { sequence }, timescale);
		let now = tokio::time::Instant::now();
		self.max_sequence = Some(self.max_sequence.unwrap_or(0).max(sequence));
		self.groups.push_back(Some((group.clone(), now)));
		let cache = self.info.as_ref().map(|i| i.cache).unwrap_or(DEFAULT_CACHE);
		self.evict_expired(now, cache);
		Ok(group)
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct TrackProducer {
	name: Arc<str>,
	state: kio::Producer<TrackState>,
	prev_subscription: Option<Subscription>,
}

impl TrackProducer {
	/// Build a producer for the given track metadata.
	pub fn new(name: impl Into<Arc<str>>, info: impl Into<Option<TrackInfo>>) -> Self {
		let info = info.into().unwrap_or_default();
		Self {
			name: name.into(),
			state: kio::Producer::new(TrackState {
				info: Some(info),
				..Default::default()
			}),
			prev_subscription: None,
		}
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	/// Create a new group with the given sequence number.
	pub fn create_group(&mut self, group: Group) -> Result<GroupProducer> {
		let mut state = self.modify()?;
		if let Some(fin) = state.final_sequence
			&& group.sequence >= fin
		{
			return Err(Error::Closed);
		}
		let info = state.info.as_ref().unwrap();
		let timescale = info.timescale;
		let cache = info.cache;

		let group = GroupProducer::new_with_timescale(group, timescale);
		if !state.duplicates.insert(group.sequence) {
			return Err(Error::Duplicate);
		}

		let now = tokio::time::Instant::now();
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.sequence));
		state.groups.push_back(Some((group.clone(), now)));
		state.evict_expired(now, cache);

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

		let info = state.info.as_ref().unwrap();
		let timescale = info.timescale;
		let cache = info.cache;

		let group = GroupProducer::new_with_timescale(Group { sequence }, timescale);

		let now = tokio::time::Instant::now();
		state.duplicates.insert(sequence);
		state.max_sequence = Some(sequence);
		state.groups.push_back(Some((group.clone(), now)));
		state.evict_expired(now, cache);

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
	/// Child groups are independent and must be aborted separately if desired;
	/// existing group consumers can still finish reading any groups that were
	/// already created.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut guard = self.modify()?;
		guard.abort = Some(err);
		guard.close();
		Ok(())
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

	/// Return the latest sequence number successfully appended to the track.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().max_sequence
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			name: self.name.clone(),
			state: self.state.weak(),
		}
	}

	/// Get a consumer handle for this in-process track.
	///
	/// Unlike a wire subscription, the info is already known, so a subscription
	/// opened from this handle resolves immediately.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			name: self.name.clone(),
			state: self.state.consume(),
		}
	}

	/// Subscribe to this in-process track, resolving synchronously.
	///
	/// The info is fixed at creation, so there's nothing to wait for (no
	/// SUBSCRIBE_OK round trip). The subscriber's stale window is clamped to the
	/// track's cache. Pass `None` for [`Subscription::default`].
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> TrackSubscriber {
		let mut preferences = subscription.into().unwrap_or_default();

		let mut state = self.modify().expect("track producer state is never closed");
		let info = state.info.clone().expect("producer always has info");
		preferences.stale = info.clamp_stale(preferences.stale);
		let subscription = kio::Producer::new(preferences);
		state.subscriptions.push(subscription.consume());
		drop(state);

		TrackSubscriber {
			name: self.name.clone(),
			info,
			state: self.state.consume(),
			subscription,
			index: 0,
			min_sequence: 0,
			next_sequence: 0,
			end_sequence: None,
		}
	}

	/// Block until the aggregate subscription changes, then return the new value.
	///
	/// Yields the most demanding request across all live subscribers, or `None`
	/// once the last one drops. Used by relays to forward downstream demand
	/// upstream (e.g. SUBSCRIBE_UPDATE).
	pub async fn subscription_changed(&mut self) -> Result<Option<Subscription>> {
		kio::wait(|waiter| self.poll_subscription_changed(waiter)).await
	}

	/// A non-blocking snapshot of the current aggregate subscription, or `None`
	/// when there are no live subscribers. Unlike [`Self::subscription`], this
	/// doesn't wait for a change or advance the change cursor.
	pub fn subscription(&self) -> Option<Subscription> {
		let state = self.state.read();
		let mut combined: Option<Subscription> = None;
		for sub in &state.subscriptions {
			if let Poll::Ready(merged) = sub.read().poll_combined(&combined) {
				combined = Some(merged);
			}
		}
		combined
	}

	pub fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Subscription>>> {
		let prev = &self.prev_subscription;
		let combined = match self
			.state
			.poll(waiter, |state| poll_combined_subscriptions(state, waiter, prev))
		{
			Poll::Ready(Ok(combined)) => combined,
			Poll::Ready(Err(state)) => return Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
			Poll::Pending => return Poll::Pending,
		};
		self.prev_subscription = combined.clone();
		Poll::Ready(Ok(combined))
	}

	/// Block until a consumer fetches a group that isn't cached, returning the request.
	///
	/// The producer serves it by making the group available (a relay issues a wire
	/// FETCH then [`create_group`](Self::create_group); an origin already has it
	/// cached, so the fetch resolves without ever reaching here). Errors once the
	/// track is aborted.
	pub async fn requested_fetch(&mut self) -> Result<FetchRequest> {
		kio::wait(|waiter| self.poll_requested_fetch(waiter)).await
	}

	pub fn poll_requested_fetch(&mut self, waiter: &kio::Waiter) -> Poll<Result<FetchRequest>> {
		match self.state.poll(waiter, |state| {
			// Only take the `&mut` (which flags the state modified) once we actually
			// have a request to pop, so idle polls don't wake unrelated waiters.
			if !state.fetches.is_empty() {
				return Poll::Ready(Ok(state.fetches.pop_front().unwrap()));
			}
			match &state.abort {
				Some(err) => Poll::Ready(Err(err.clone())),
				None => Poll::Pending,
			}
		}) {
			Poll::Ready(Ok(res)) => Poll::Ready(res),
			Poll::Ready(Err(state)) => Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
			Poll::Pending => Poll::Pending,
		}
	}

	fn modify(&self) -> Result<kio::Mut<'_, TrackState>> {
		TrackState::modify(&self.state)
	}
}

/// Aggregate every live subscriber's preferences into the most demanding request,
/// returning `Poll::Ready` only when the result differs from `prev`.
///
/// Iterates `subscriptions` immutably so it never flags the [`TrackState`] as
/// modified on a no-op poll. Marking it modified would drain and wake unrelated
/// waiters on the channel (e.g. a [`TrackSubscriberPending`] parked on track
/// info), which races with [`TrackRequest::accept`] and can drop that wakeup.
/// Closed subscribers are pruned only when one has actually closed, which is a
/// real change that legitimately wakes other waiters.
fn poll_combined_subscriptions(
	state: &mut kio::Mut<'_, TrackState>,
	waiter: &kio::Waiter,
	prev: &Option<Subscription>,
) -> Poll<Option<Subscription>> {
	let mut combined = None;
	let mut any_closed = false;
	for sub in state.subscriptions.iter() {
		match sub.poll(waiter, |sub| sub.poll_combined(&combined)) {
			Poll::Ready(Ok(sub)) => combined = Some(sub),
			Poll::Ready(Err(_)) => any_closed = true,
			Poll::Pending => {}
		}
	}

	if any_closed {
		state.subscriptions.retain(|sub| !sub.is_closed());
	}

	if &combined == prev {
		Poll::Pending
	} else {
		Poll::Ready(combined)
	}
}

/// A weak reference to a track that doesn't prevent auto-close.
#[derive(Clone)]
pub(crate) struct TrackWeak {
	name: Arc<str>,
	state: kio::Weak<TrackState>,
}

impl TrackWeak {
	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			name: self.name.clone(),
			state: self.state.consume(),
		}
	}

	/// The shared name handle, for use as a broadcast lookup key (clone is a
	/// refcount bump, and the same `Arc` is shared with the track's handles).
	pub(crate) fn name(&self) -> &Arc<str> {
		&self.name
	}
}

/// A handle to a single track within a broadcast.
///
/// Obtained from [`crate::BroadcastConsumer::track`]. Holding it sends nothing
/// to the publisher; it just names a track you can [`subscribe`](Self::subscribe)
/// to (a live, ongoing stream of groups) later. The same handle can be subscribed
/// to multiple times, and clones are cheap.
// TODO: add `fetch` for one-shot retrieval of a past group range.
#[derive(Clone)]
pub struct TrackConsumer {
	name: Arc<str>,
	state: kio::Consumer<TrackState>,
}

impl TrackConsumer {
	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	pub(crate) fn weak(&self) -> TrackWeak {
		TrackWeak {
			name: self.name.clone(),
			state: self.state.weak(),
		}
	}

	/// Open a live subscription.
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> Result<TrackSubscriberPending> {
		let subscription = kio::Producer::new(subscription.into().unwrap_or_default());

		match self.state.write() {
			Ok(mut state) => state.subscriptions.push(subscription.consume()),
			Err(state) => return Err(state.abort.clone().unwrap_or(Error::Dropped)),
		};

		Ok(TrackSubscriberPending {
			name: self.name.clone(),
			state: self.state.clone(),
			subscription,
			waiter: None,
		})
	}

	/// Fetch a single past group, without holding a live subscription.
	///
	/// Returns a [`TrackFetchPending`] that resolves to the [`GroupConsumer`] once
	/// the group is available: immediately if it's already cached, otherwise once
	/// the producer serves the request (a wire FETCH for a relay). `options` accepts
	/// `None`, a [`Fetch`], or `Fetch::default()`. The pending resolves to
	/// [`Error::NotFound`] if the group can never exist (past the final sequence).
	pub fn fetch(&self, sequence: u64, options: impl Into<Option<Fetch>>) -> Result<TrackFetchPending> {
		let options = options.into().unwrap_or_default();

		match self.state.write() {
			Ok(mut state) => {
				// Only signal the producer when the group isn't already determined by
				// the cache (cached, evicted-past-final, or aborted); otherwise the
				// pending resolves straight from `poll_get_group` with no wire fetch.
				// TODO: This is a tiny bit racey
				if state.poll_get_group(sequence).is_pending() {
					state.fetches.push_back(FetchRequest {
						sequence,
						priority: options.priority,
					});
				}
			}
			Err(state) => return Err(state.abort.clone().unwrap_or(Error::Dropped)),
		};

		Ok(kio::Pending::new(TrackFetch {
			state: self.state.clone(),
			sequence,
		}))
	}

	pub fn info(&self) -> TrackInfoPending {
		TrackInfoPending {
			state: self.state.clone(),
			waiter: None,
		}
	}
}

pub struct TrackSubscriberPending {
	name: Arc<str>,
	state: kio::Consumer<TrackState>,
	subscription: kio::Producer<Subscription>,
	// Kept alive between `Future::poll` calls so the weak waker kio registered
	// stays upgradeable until the next poll replaces it. A temporary would drop
	// after poll returns, leaving a dead weak ref and a lost wakeup on accept.
	waiter: Option<kio::Waiter>,
}

impl TrackSubscriberPending {
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<TrackSubscriber>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;

		Poll::Ready(Ok(TrackSubscriber {
			name: self.name.clone(),
			info,
			state: self.state.clone(),
			subscription: self.subscription.clone(),
			index: 0,
			min_sequence: 0,
			next_sequence: 0,
			end_sequence: None,
		}))
	}

	pub fn update(&mut self, subscription: Subscription) {
		if let Ok(mut state) = self.subscription.write() {
			*state = subscription;
		} else {
			panic!("subscription is closed");
		}
	}
}

impl Future for TrackSubscriberPending {
	type Output = Result<TrackSubscriber>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		// Replacing drops the previous waiter, freeing its slot so the register
		// call below can recycle it (see kio's weak-waker GC).
		this.waiter = Some(kio::Waiter::new(cx.waker().clone()));
		this.poll_ok(this.waiter.as_ref().unwrap())
	}
}

pub struct TrackInfoPending {
	state: kio::Consumer<TrackState>,
	// See [`TrackSubscriberPending::waiter`]: kept alive so the registered weak
	// waker stays upgradeable between polls.
	waiter: Option<kio::Waiter>,
}

impl TrackInfoPending {
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<TrackInfo>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;
		Poll::Ready(Ok(info))
	}
}

impl Future for TrackInfoPending {
	type Output = Result<TrackInfo>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		this.waiter = Some(kio::Waiter::new(cx.waker().clone()));
		this.poll_ok(this.waiter.as_ref().unwrap())
	}
}

/// Options for a one-shot [`TrackConsumer::fetch`] of a past group.
#[derive(Clone, Debug, Default)]
pub struct Fetch {
	/// Delivery priority for the fetched group's stream. Defaults to 0.
	pub priority: u8,
}

impl Fetch {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: u8) -> Self {
		self.priority = priority;
		self
	}
}

/// A specific group requested via [`TrackConsumer::fetch`], handed to the producer
/// (or a relay) to serve via [`TrackProducer::requested_fetch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchRequest {
	/// The group sequence the consumer wants.
	pub sequence: u64,
	/// The requested delivery priority.
	pub priority: u8,
}

/// The pollable state of a [`TrackConsumer::fetch`].
///
/// Awaited via the [`TrackFetchPending`] wrapper; resolves to the
/// [`GroupConsumer`] once the group lands in the track's cache (already present,
/// or produced after a wire FETCH), or [`Error::NotFound`] if it can never exist.
pub struct TrackFetch {
	state: kio::Consumer<TrackState>,
	sequence: u64,
}

impl kio::Future for TrackFetch {
	type Output = Result<GroupConsumer>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		let group = ready!(self.state.poll(waiter, |state| state.poll_get_group(self.sequence)))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;

		// `poll_get_group` only returns `None` once the group is at/past the
		// final sequence, so it can never be produced.
		Poll::Ready(group.ok_or(Error::NotFound))
	}
}

/// A pending fetch returned by [`TrackConsumer::fetch`]. `.await` it for the
/// [`GroupConsumer`].
pub type TrackFetchPending = kio::Pending<TrackFetch>;

/// A live subscription to a track, used to read its groups.
///
/// Created via [`TrackConsumer::subscribe`](crate::TrackConsumer::subscribe), or
/// directly from a [`TrackProducer`] for an in-process track. Carries this
/// subscriber's [`Subscription`] preferences, which feed the producer's aggregate.
pub struct TrackSubscriber {
	name: Arc<str>,
	info: TrackInfo,
	state: kio::Consumer<TrackState>,

	subscription: kio::Producer<Subscription>,
	/// Arrival-order cursor used by [`Self::recv_group`].
	index: usize,
	/// Minimum sequence to return from any `recv` method. Set by [`Self::start_at`].
	min_sequence: u64,
	/// One past the highest sequence returned by [`Self::next_group`].
	/// Used only by that method to skip late arrivals; does not affect [`Self::recv_group`].
	next_sequence: u64,
	/// Inclusive upper sequence bound for [`Self::next_group`]. `None` means
	/// no cap. Set by [`Self::end_at`]; can be raised, lowered, or unset at
	/// any time. Groups beyond the cap stay in the producer's cache and
	/// become eligible again when the cap rises (or is removed).
	end_sequence: Option<u64>,
}

impl TrackSubscriber {
	pub fn info(&self) -> &TrackInfo {
		&self.info
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&kio::Ref<'_, TrackState>) -> Poll<Result<R>>,
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
	///
	/// Honors the cap set by [`Self::end_at`]: groups with sequence past the cap are left
	/// in the producer's cache and become eligible again if the cap is raised or removed.
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<GroupConsumer>>> {
		let floor = self.next_sequence.max(self.min_sequence);
		let Some(group) = ready!(self.poll(waiter, |state| state.poll_next_in_range(floor, self.end_sequence))?) else {
			return Poll::Ready(Ok(None));
		};
		self.next_sequence = group.sequence.saturating_add(1);
		Poll::Ready(Ok(Some(group)))
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

	/// Whether `other` was cloned from this subscriber (shares the same underlying state).
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

	/// Cap the consumer at the specified sequence (inclusive), or remove the cap entirely.
	///
	/// Accepts a bare `u64` (cap), `Some(u64)`, or `None` (uncap).
	///
	/// Affects [`Self::next_group`] only: groups beyond the cap stay in the producer's
	/// cache rather than being skipped past, so a later call to [`Self::end_at`] with a
	/// higher value (or `None`) makes them available again. Lowering the cap below the
	/// consumer's current cursor parks the consumer until the cap is raised.
	pub fn end_at(&mut self, sequence: impl Into<Option<u64>>) {
		self.end_sequence = sequence.into();
	}

	/// This subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.subscription.read().clone()
	}

	/// Replace this subscriber's preferences, updating the producer's aggregate.
	pub fn update(&mut self, subscription: Subscription) {
		if let Ok(mut state) = self.subscription.write() {
			*state = subscription;
		} else {
			panic!("subscription is closed");
		}
	}

	/// Return the latest sequence number in the track.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().max_sequence
	}
}

pub struct TrackRequest {
	name: Arc<str>,
	state: kio::Producer<TrackState>,

	// The previous subscription that was combined, used to detect changes.
	prev_subscription: Option<Subscription>,
}

impl TrackRequest {
	pub fn new(name: impl Into<Arc<str>>) -> Self {
		Self {
			name: name.into(),
			state: Default::default(),
			prev_subscription: None,
		}
	}

	/// The requested track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			name: self.name.clone(),
			state: self.state.consume(),
		}
	}

	/// Block until a consumer fetches a group, returning the request to serve.
	///
	/// A relay serves it by opening a wire FETCH and calling [`Self::serve_fetch`].
	/// Unlike [`Self::accept`], this doesn't consume the request, so the same track
	/// can serve any number of fetches (and later a subscription). Errors once the
	/// track is aborted.
	pub async fn requested_fetch(&self) -> Result<FetchRequest> {
		kio::wait(|waiter| self.poll_requested_fetch(waiter)).await
	}

	pub fn poll_requested_fetch(&self, waiter: &kio::Waiter) -> Poll<Result<FetchRequest>> {
		match self.state.poll(waiter, |state| {
			// Only take the `&mut` (which flags the state modified) once there's a
			// request to pop, so idle polls don't wake unrelated waiters.
			if !state.fetches.is_empty() {
				return Poll::Ready(Ok(state.fetches.pop_front().unwrap()));
			}
			match &state.abort {
				Some(err) => Poll::Ready(Err(err.clone())),
				None => Poll::Pending,
			}
		}) {
			Poll::Ready(Ok(res)) => Poll::Ready(res),
			// A TrackRequest holds the only producer, so the channel can't close here.
			Poll::Ready(Err(_)) => Poll::Pending,
			Poll::Pending => Poll::Pending,
		}
	}

	/// Make a fetched group available in the track's cache, resolving the matching
	/// [`TrackConsumer::fetch`]. Returns a [`GroupProducer`] to fill from the wire.
	///
	/// `timescale` is the scale from the wire FETCH_OK, used for the group's frames.
	/// Returns [`Error::Duplicate`] if the group is already present.
	pub fn serve_fetch(&self, sequence: u64, timescale: impl Into<Option<Timescale>>) -> Result<GroupProducer> {
		TrackState::modify(&self.state)?.insert_fetch_group(sequence, timescale.into())
	}

	/// Poll for the request becoming unused (every consumer dropped), so a relay can
	/// stop serving and drop the request.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}

	/// Serve the request with the given track, resolving every waiting subscriber.
	///
	/// The track's name must match [`Self::name`]. Returns [`Error::NotFound`] on
	/// mismatch, or the broadcast's abort error if it closed while pending.
	pub fn accept(self, info: impl Into<Option<TrackInfo>>) -> TrackProducer {
		self.state.write().ok().unwrap().info = Some(info.into().unwrap_or_default());
		TrackProducer {
			name: self.name,
			state: self.state,
			prev_subscription: None,
		}
	}

	/// Reject the request, waking all waiting subscribers with `err`.
	pub fn reject(self, err: Error) {
		if let Ok(mut state) = self.state.write() {
			state.abort = Some(err);
		}
	}

	pub fn subscription(&self) -> Option<Subscription> {
		let state = self.state.read();
		let mut combined: Option<Subscription> = None;
		for sub in &state.subscriptions {
			if let Poll::Ready(merged) = sub.read().poll_combined(&combined) {
				combined = Some(merged);
			}
		}
		combined
	}

	pub async fn subscription_changed(&mut self) -> Option<Subscription> {
		kio::wait(|waiter| self.poll_subscription_changed(waiter)).await
	}

	pub fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Option<Subscription>> {
		let prev = &self.prev_subscription;
		// The request owns the only producer, so the channel can't be closed here.
		let combined = match ready!(
			self.state
				.poll(waiter, |state| poll_combined_subscriptions(state, waiter, prev))
		) {
			Ok(combined) => combined,
			Err(_) => unreachable!("a TrackRequest holds the only producer"),
		};
		self.prev_subscription = combined.clone();
		Poll::Ready(combined)
	}

	pub(super) fn weak(&self) -> TrackWeak {
		TrackWeak {
			name: self.name.clone(),
			state: self.state.weak(),
		}
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
	fn live_groups(state: &TrackState) -> usize {
		state.groups.iter().flatten().count()
	}

	/// Helper: get the sequence number of the first live group.
	fn first_live_sequence(state: &TrackState) -> u64 {
		state.groups.iter().flatten().next().unwrap().0.sequence
	}

	#[tokio::test]
	async fn evict_expired_groups() {
		tokio::time::pause();

		let mut producer = TrackProducer::new("test", None);

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
		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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

		let mut producer = TrackProducer::new("test", None);
		producer.append_group().unwrap(); // seq 0

		// Advance time past threshold.
		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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

		let mut producer = TrackProducer::new("test", None);
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

		let mut producer = TrackProducer::new("test", None);
		producer.append_group().unwrap(); // seq 0

		let mut consumer = producer.subscribe(None);

		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;
		producer.append_group().unwrap(); // seq 1

		// Group 0 was evicted. Consumer should get group 1.
		let group = consumer.assert_group();
		assert_eq!(group.sequence, 1);
	}

	#[tokio::test]
	async fn cache_age_controls_eviction() {
		tokio::time::pause();

		// A shorter cache evicts sooner than the default.
		let mut producer = TrackProducer::new("test", TrackInfo::default().with_cache(Duration::from_secs(1)));
		producer.append_group().unwrap(); // seq 0

		// Past the custom cache but well within DEFAULT_CACHE.
		tokio::time::advance(Duration::from_secs(2)).await;
		producer.append_group().unwrap(); // seq 1

		// Seq 0 is gone because the publisher only keeps groups for 1s.
		let state = producer.state.read();
		assert_eq!(live_groups(&state), 1);
		assert_eq!(first_live_sequence(&state), 1);
	}

	#[test]
	fn stale_clamped_to_cache() {
		let producer = TrackProducer::new("test", TrackInfo::default().with_cache(Duration::from_secs(2)));

		// A stale window beyond the cache is capped to the cache; a group can't be
		// waited for longer than the publisher keeps it.
		let mut subscriber = producer.subscribe(Subscription::default().with_stale(Duration::from_secs(10)));
		assert_eq!(subscriber.subscription().stale, Duration::from_secs(2));

		// A window within the cache is left alone, and ZERO (skip immediately) stays ZERO.
		subscriber.update(Subscription::default().with_stale(Duration::from_millis(500)));
		assert_eq!(subscriber.subscription().stale, Duration::from_millis(500));

		subscriber.update(Subscription::default().with_stale(Duration::ZERO));
		assert_eq!(subscriber.subscription().stale, Duration::ZERO);
	}

	#[tokio::test]
	async fn out_of_order_max_sequence_at_front() {
		tokio::time::pause();

		let mut producer = TrackProducer::new("test", None);

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
		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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

		let mut producer = TrackProducer::new("test", None);

		// Arrive: seq 5, then seq 3.
		producer.create_group(Group { sequence: 5 }).unwrap();

		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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
		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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
		let mut consumer = producer.subscribe(None);
		let group = consumer.assert_group();
		// consume() starts at index 0, first non-tombstoned group is seq 5.
		assert_eq!(group.sequence, 5);
	}

	#[test]
	fn append_finish_cannot_be_rewritten() {
		let mut producer = TrackProducer::new("test", None);

		// Finishing an empty track is valid (fin = 0, total groups = 0).
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn finish_after_groups() {
		let mut producer = TrackProducer::new("test", None);

		producer.append_group().unwrap();
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn insert_finish_validates_sequence_and_freezes_to_max() {
		let mut producer = TrackProducer::new("test", None);
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
		let mut producer = TrackProducer::new("test", None);
		producer.create_group(Group { sequence: 1 }).unwrap();
		producer.finish_at(1).unwrap();

		let mut consumer = producer.subscribe(None);
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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
	async fn next_group_and_recv_group_use_independent_cursors() {
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

		// Out-of-order arrivals: seq 5 first, then seq 3.
		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();

		// next_group is sequence-ordered: it returns the smallest sequence first,
		// regardless of arrival order.
		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 3);

		// recv_group is arrival-ordered and uses an independent cursor, so it
		// still starts at the first arrival.
		assert_eq!(consumer.assert_group().sequence, 5);
	}

	#[tokio::test]
	async fn end_at_caps_next_group() {
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

		for s in 0..6 {
			producer.create_group(Group { sequence: s }).unwrap();
		}

		consumer.end_at(2);

		// Groups 0, 1, 2 are within the cap.
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);

		// Group 3 is beyond the cap: next_group parks even though cached groups exist.
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"capped consumer must block instead of returning out-of-range groups"
		);
	}

	#[tokio::test]
	async fn end_at_release_drains_cached_groups() {
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

		for s in 0..6 {
			producer.create_group(Group { sequence: s }).unwrap();
		}

		consumer.end_at(1);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert!(consumer.next_group().now_or_never().is_none(), "capped at 1");

		// Raise the cap; previously-blocked cached groups become available again.
		consumer.end_at(4);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			3
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			4
		);
		assert!(consumer.next_group().now_or_never().is_none(), "capped at 4");

		// Remove the cap; everything remaining flows.
		consumer.end_at(None);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			5
		);
		assert!(consumer.next_group().now_or_never().is_none(), "no more groups");
	}

	#[tokio::test]
	async fn end_at_lower_than_cursor_parks_consumer() {
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

		for s in 0..3 {
			producer.create_group(Group { sequence: s }).unwrap();
		}

		// Drain everything with no cap.
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);

		// Lower the cap below the cursor. New groups beyond the cap are blocked.
		consumer.end_at(1);
		producer.create_group(Group { sequence: 3 }).unwrap();
		producer.create_group(Group { sequence: 4 }).unwrap();
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"cap is below cursor; nothing returnable until cap rises"
		);

		// Restoring the cap to no-limit (or any value >= cursor) releases them.
		consumer.end_at(None);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			3
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			4
		);
	}

	#[tokio::test]
	async fn end_at_toggling_around_late_arrivals() {
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

		consumer.end_at(5);

		// Out-of-order arrivals all within the cap.
		producer.create_group(Group { sequence: 2 }).unwrap();
		producer.create_group(Group { sequence: 5 }).unwrap();
		producer.create_group(Group { sequence: 3 }).unwrap();
		// One beyond the cap; should be held even though it arrived in the middle.
		producer.create_group(Group { sequence: 8 }).unwrap();
		producer.create_group(Group { sequence: 4 }).unwrap();

		// next_group walks in sequence order through everything <= cap.
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			3
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			4
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			5
		);
		// Now blocked: 8 is still beyond the cap.
		assert!(consumer.next_group().now_or_never().is_none());

		// Raise the cap; cached seq 8 is finally served.
		consumer.end_at(10);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			8
		);
	}

	#[tokio::test]
	async fn read_frame_returns_single_frame_per_group() {
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);
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
		let mut producer = TrackProducer::new("test", None);
		let mut consumer = producer.subscribe(None);

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
		let mut producer = TrackProducer::new("test", None);
		producer.create_group(Group { sequence: 1 }).unwrap();
		producer.finish_at(1).unwrap();

		let consumer = producer.subscribe(None);
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
		let mut producer = TrackProducer::new("test", None);
		{
			let mut state = producer.state.write().ok().unwrap();
			state.max_sequence = Some(u64::MAX);
		}

		assert!(matches!(producer.append_group(), Err(Error::BoundsExceeded(_))));
	}

	#[tokio::test]
	async fn fetch_cache_hit() {
		let mut producer = TrackProducer::new("test", None);

		// Produce a cached group.
		let mut group = producer.append_group().unwrap(); // seq 0
		group.write_frame(bytes::Bytes::from_static(b"hello")).unwrap();
		group.finish().unwrap();

		// A cached group resolves immediately and never signals the producer.
		let consumer = producer.consume();
		let mut g = consumer.fetch(0, None).unwrap().await.unwrap();
		assert_eq!(g.sequence, 0);
		assert_eq!(&g.read_frame().await.unwrap().unwrap()[..], b"hello");

		// Nothing was queued for the producer to serve.
		assert!(producer.poll_requested_fetch(&kio::Waiter::noop()).is_pending());
	}

	#[tokio::test]
	async fn fetch_miss_signals_producer() {
		let mut producer = TrackProducer::new("test", None);
		let consumer = producer.consume();

		// The group isn't cached yet, so the fetch stays pending and queues a request.
		// `*pending` derefs the wrapper to the inner `TrackFetch` (a `kio::Future`).
		let mut pending = consumer.fetch(5, Fetch::default().with_priority(7)).unwrap();
		assert!(kio::Future::poll(&mut *pending, &kio::Waiter::noop()).is_pending());

		let req = producer
			.requested_fetch()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(
			req,
			FetchRequest {
				sequence: 5,
				priority: 7
			}
		);

		// Serve it by producing the group; the fetch then resolves.
		let mut group = producer.create_group(Group { sequence: 5 }).unwrap();
		group.write_frame(bytes::Bytes::from_static(b"hi")).unwrap();
		group.finish().unwrap();

		let mut g = pending.await.unwrap();
		assert_eq!(g.sequence, 5);
		assert_eq!(&g.read_frame().await.unwrap().unwrap()[..], b"hi");
	}

	#[tokio::test]
	async fn fetch_past_final_not_found() {
		let mut producer = TrackProducer::new("test", None);
		producer.append_group().unwrap(); // seq 0
		producer.finish().unwrap(); // final_sequence = 1

		// A group at or past the final sequence can never exist.
		let consumer = producer.consume();
		assert!(matches!(consumer.fetch(5, None).unwrap().await, Err(Error::NotFound)));

		// And it doesn't signal the producer.
		assert!(producer.poll_requested_fetch(&kio::Waiter::noop()).is_pending());
	}

	#[tokio::test]
	async fn fetch_aborts_with_track() {
		let mut producer = TrackProducer::new("test", None);
		let consumer = producer.consume();

		let mut pending = consumer.fetch(3, None).unwrap();
		assert!(kio::Future::poll(&mut *pending, &kio::Waiter::noop()).is_pending());

		producer.abort(Error::Cancel).unwrap();
		assert!(pending.await.is_err());
	}
}
