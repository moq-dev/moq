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

use super::cache::{self, Cache};
use super::{Fetch, Group, GroupConsumer, GroupProducer};

use std::{
	collections::{HashSet, VecDeque},
	sync::Arc,
	task::{Poll, ready},
	time::Duration,
};

/// Default local retention window for cached groups: 5 seconds.
///
/// This is the default [`crate::cache::Config::max_age`], so every broadcast and bare track keeps
/// roughly the last 5 seconds of groups unless an explicit [`Cache`] overrides it. Not carried on
/// the wire: retention is a local policy, not a publisher guarantee.
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
	/// The publisher honors it by negotiating a codec in TRACK_INFO; codec-less
	/// peers (older drafts) ignore it and send frames verbatim.
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "std::ops::Not::not"))]
	pub compress: bool,
	/// Units per second for per-frame timestamps on this track.
	///
	/// `None` means the publisher hasn't advertised a timescale; subscribers
	/// receive frames with `timestamp: None`. On Lite05+ a `Some(_)` value is
	/// reported in TRACK_INFO and the publisher zigzag-delta encodes
	/// per-frame timestamps at that scale on the wire; rejecting a frame at
	/// the wrong scale prevents silent corruption.
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub timescale: Option<Timescale>,
	/// The publisher's priority for this track, used only to break ties between
	/// subscriptions of equal subscriber priority. Reported in TRACK_INFO (Lite05+);
	/// kept out of the catalog (a transport property, not media metadata).
	#[cfg_attr(feature = "serde", serde(skip))]
	pub priority: u8,
	/// The publisher's group ordering preference (newest-first when `false`), used
	/// only to break ties. Reported in TRACK_INFO (Lite05+); kept out of the catalog.
	#[cfg_attr(feature = "serde", serde(skip))]
	pub ordered: bool,
}

impl Default for TrackInfo {
	fn default() -> Self {
		Self {
			compress: false,
			timescale: None,
			priority: 0,
			ordered: true,
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

	/// Set the publisher's tie-break priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: u8) -> Self {
		self.priority = priority;
		self
	}

	/// Set the publisher's group ordering preference, returning `self` for chaining.
	pub fn with_ordered(mut self, ordered: bool) -> Self {
		self.ordered = ordered;
		self
	}
}

/// Clamp a subscriber's stale window to a track's local retention.
///
/// A subscriber can't usefully wait for a late group longer than the track keeps it around. The
/// bound is the attached [`Cache`]'s `max_age` (the 5-second default unless overridden). A cache
/// with `max_age == Duration::ZERO` is latest-group-only, so the window collapses to
/// `Duration::ZERO` (skip immediately). With no cache at all the bound is also `Duration::ZERO`.
fn clamp_stale(cache: Option<&Cache>, stale: Duration) -> Duration {
	let bound = cache.map(|c| c.max_age()).unwrap_or(Duration::ZERO);
	stale.min(bound)
}

/// A cached group plus its registration in the shared [`Cache`], if any.
///
/// A group is registered with the cache only once it stops being the latest; the latest group
/// is never handed to the cache (a live subscriber must always reach it). `token` is `Some`
/// once the group has been registered.
struct Cached {
	group: GroupProducer,
	created_at: web_async::time::Instant,
	token: Option<cache::Token>,
}

#[derive(Default)]
struct TrackState {
	// The info for the track; always Some for TrackSubscriber/TrackProducer.
	info: Option<TrackInfo>,

	// Groups in arrival order. `None` entries are tombstones for evicted groups.
	groups: VecDeque<Option<Cached>>,

	// Shared RAM cache governing retention of non-latest groups. `TrackProducer::new` installs a
	// default (5s, no byte cap); `None` (e.g. a default-constructed state) keeps only the latest
	// group, dropping every superseded group at once.
	cache: Option<Cache>,

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

	// Specific groups requested via `fetch` that aren't cached yet, FIFO for a
	// `TrackDynamic` to serve (see `TrackDynamic::requested_group`).
	fetches: VecDeque<GroupRequested>,

	// Number of live `TrackDynamic` handles. While zero, the track serves no
	// uncached groups, so a cache-miss `fetch` on an accepted track fails fast
	// instead of blocking forever (mirrors `BroadcastState::dynamic`).
	dynamic: usize,
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
			if let Some(cached) = slot
				&& cached.group.sequence >= min_sequence
				&& self.touch(cached)
			{
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
			if group.sequence < next_sequence {
				continue;
			}

			let mut consumer = group.consume();
			match consumer.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					// A frame read keeps the group recent (and evicts it first if already aged
					// out, in which case we skip it). Without touching here, a group read only
					// at the frame level would age out wrongly.
					if !self.touch(cached) {
						continue;
					}
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

		let mut best: Option<&Cached> = None;
		for cached in self.groups.iter().flatten() {
			let group = &cached.group;
			if group.sequence < next_sequence {
				continue;
			}
			if let Some(end) = end_sequence
				&& group.sequence > end
			{
				continue;
			}
			if best.is_none_or(|b| group.sequence < b.group.sequence) {
				best = Some(cached);
			}
		}

		if let Some(cached) = best
			&& self.touch(cached)
		{
			return Poll::Ready(Ok(Some(cached.group.consume())));
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

	/// Find a cached group by sequence, skipping tombstones. Synchronous, never blocks.
	///
	/// Records the access (touch-before-evict): a group already past the cache's `max_age` is
	/// evicted here rather than revived, so it reads as a miss.
	fn cached_group(&self, sequence: u64) -> Option<GroupConsumer> {
		let cached = self.groups.iter().flatten().find(|c| c.group.sequence == sequence)?;
		self.touch(cached).then(|| cached.group.consume())
	}

	fn poll_get_group(&self, sequence: u64) -> Poll<Result<Option<GroupConsumer>>> {
		if let Some(group) = self.cached_group(sequence) {
			return Poll::Ready(Ok(Some(group)));
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

	/// Resolve a one-shot fetch: the cached group, or an [`Error`] once it can never
	/// be served. Unlike [`Self::poll_get_group`] there's no `Ok(None)`, since a
	/// missing group is a failure ([`Error::NotFound`]), not an end-of-stream.
	///
	/// A miss is unservable when the group is past the final sequence, or when no
	/// [`TrackDynamic`] exists to fetch old content (`dynamic == 0`). On-demand tracks
	/// (from a [`TrackRequest`]) are dynamic from creation, so a relay's fetch waits to
	/// be served rather than racing the handler into existence.
	fn poll_fetch(&self, sequence: u64) -> Poll<Result<GroupConsumer>> {
		if let Some(group) = self.cached_group(sequence) {
			return Poll::Ready(Ok(group));
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}

		// Past the final sequence, or no handler to serve old content: unservable.
		let past_final = self.final_sequence.is_some_and(|fin| sequence >= fin);
		if past_final || self.dynamic == 0 {
			return Poll::Ready(Err(Error::NotFound));
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
	/// track's insert) is tombstoned so consumers skip it.
	fn retain(&mut self, now: web_async::time::Instant) {
		let max_sequence = self.max_sequence;

		for slot in self.groups.iter_mut() {
			let Some(cached) = slot else { continue };

			// Never evict the current latest group.
			if Some(cached.group.sequence) == max_sequence {
				continue;
			}

			match &self.cache {
				None => {
					// Latest-only: a superseded group is dropped immediately. Abort it first so
					// any parked reader surfaces `Error::Old` instead of hanging on a frame that
					// will never arrive.
					self.duplicates.remove(&cached.group.sequence);
					let _ = cached.group.abort(Error::Old);
					*slot = None;
				}
				Some(cache) => {
					// Hand the group to the shared budget the first time it is superseded.
					if cached.token.is_none() {
						let bytes = cached.group.cached_size();
						cached.token = Some(cache.insert(cached.group.clone(), bytes, cached.created_at));
					}
				}
			}
		}

		// Run age/byte eviction on the shared budget now, so an active track ages out stale
		// groups (its own and its peers') even when no new group needed registering this round.
		if let Some(cache) = &self.cache {
			cache.evict(now);
		}

		// The shared cache may have aborted some of our (or another track's) groups; tombstone
		// any that are now aborted so consumers skip them and the budget bookkeeping matches.
		for slot in self.groups.iter_mut() {
			if let Some(cached) = slot
				&& Some(cached.group.sequence) != max_sequence
				&& cached.group.is_aborted()
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

	/// Record a read of a cached group as a wall-clock access, returning whether the group is
	/// still valid afterward (i.e. the read should be served).
	///
	/// Touch-before-evict: the shared cache runs eviction before refreshing recency, so a group
	/// already past `max_age` is dropped here rather than revived by the read. Returns `false`
	/// when the group is gone (evicted now or earlier, or otherwise aborted) so the caller
	/// treats it as a miss. The latest group (no token) and the no-cache case are always valid.
	fn touch(&self, cached: &Cached) -> bool {
		match (&self.cache, cached.token) {
			(Some(cache), Some(token)) => cache.touch(token, web_async::time::Instant::now()),
			_ => !cached.group.is_aborted(),
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

	fn modify(producer: &kio::Producer<Self>) -> Result<kio::Mut<'_, Self>> {
		producer.write().map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Insert a group fetched for a [`GroupRequest`], setting the track's [`TrackInfo`]
	/// if it isn't accepted yet. The group's timescale comes from that info, so a
	/// fetch can serve an as-yet-unaccepted track (e.g. a relay with no live
	/// subscription). The group lands in the cache so a waiting
	/// [`TrackFetch`] resolves via [`Self::poll_fetch`].
	fn insert_group_request(&mut self, sequence: u64, info: Option<TrackInfo>) -> Result<GroupProducer> {
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

		// Adopt the supplied info only if the track hasn't been accepted yet.
		let info = self.info.get_or_insert_with(|| info.unwrap_or_default());

		let group = GroupProducer::new(Group { sequence }, info.timescale);
		let now = web_async::time::Instant::now();
		self.max_sequence = Some(self.max_sequence.unwrap_or(0).max(sequence));
		self.groups.push_back(Some(Cached {
			group: group.clone(),
			created_at: now,
			token: None,
		}));
		self.retain(now);
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
	///
	/// The track gets its own default [`Cache`] (a 5-second retention window, no byte cap), so a
	/// late subscriber can replay the last few seconds. Override it with [`Self::with_cache`], or
	/// produce the track from a [`crate::BroadcastProducer`] to share that broadcast's cache.
	pub fn new(name: impl Into<Arc<str>>, info: impl Into<Option<TrackInfo>>) -> Self {
		let info = info.into().unwrap_or_default();
		Self {
			name: name.into(),
			state: kio::Producer::new(TrackState {
				info: Some(info),
				cache: Some(Cache::new(cache::Config::default())),
				..Default::default()
			}),
			prev_subscription: None,
		}
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	/// Attach a shared [`Cache`] governing how much of this track's history is retained, replacing
	/// the track's default ([`crate::DEFAULT_CACHE`], 5 seconds, no byte cap).
	///
	/// Superseded groups are retained in RAM up to the cache's shared byte and age budget, evicted
	/// least-recently-accessed first; a cache with `max_age == Duration::ZERO` keeps only the
	/// latest group. Clone the same [`Cache`] across tracks to share one budget. Set this before
	/// producing groups; it takes effect on the next group. Returns `self` for chaining.
	pub fn with_cache(self, cache: Cache) -> Self {
		if let Ok(mut state) = self.state.write() {
			state.cache = Some(cache);
		}
		self
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

		let group = GroupProducer::new(group, timescale);
		if !state.duplicates.insert(group.sequence) {
			return Err(Error::Duplicate);
		}

		let now = web_async::time::Instant::now();
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.sequence));
		state.groups.push_back(Some(Cached {
			group: group.clone(),
			created_at: now,
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

		let info = state.info.as_ref().unwrap();
		let timescale = info.timescale;

		let group = GroupProducer::new(Group { sequence }, timescale);

		let now = web_async::time::Instant::now();
		state.duplicates.insert(sequence);
		state.max_sequence = Some(sequence);
		state.groups.push_back(Some(Cached {
			group: group.clone(),
			created_at: now,
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

	/// Create a [`TrackDemand`]: a cloneable, watch-only handle to this track's
	/// subscriber demand.
	///
	/// Lets a publisher gate work (e.g. on-demand capture) on whether anyone is
	/// subscribed, without the ability to publish frames or close the track. The
	/// handle is weak, so holding one neither keeps the track alive nor pins its
	/// cached groups.
	pub fn demand(&self) -> TrackDemand {
		TrackDemand {
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
	/// track's local retention (the attached [`Cache`]'s age bound, or `Duration::ZERO`
	/// when none). Pass `None` for [`Subscription::default`].
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> TrackSubscriber {
		let mut preferences = subscription.into().unwrap_or_default();

		let mut state = self.modify().expect("track producer state is never closed");
		let info = state.info.clone().expect("producer always has info");
		preferences.stale = clamp_stale(state.cache.as_ref(), preferences.stale);
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
		let mut combined = None;
		let mut state = match self.state.poll(waiter, |state| {
			let next = combined_subscription(state, waiter);
			if &next == prev {
				Poll::Pending
			} else {
				combined = next;
				Poll::Ready(())
			}
		}) {
			Poll::Ready(Ok(state)) => state,
			Poll::Ready(Err(state)) => return Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
			Poll::Pending => return Poll::Pending,
		};
		// The aggregate changed: prune any closed subscribers now that we hold the lock.
		state.subscriptions.retain(|sub| !sub.is_closed());
		drop(state);
		self.prev_subscription = combined.clone();
		Poll::Ready(Ok(combined))
	}

	/// Poll for the producer becoming unused (every consumer dropped).
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}

	/// Create a [`TrackDynamic`] handle that serves on-demand fetches of uncached
	/// (old) groups. Most producers never need this; a relay creates one to fetch
	/// past groups from upstream.
	pub fn dynamic(&self) -> TrackDynamic {
		TrackDynamic::new(self.name.clone(), self.state.clone())
	}

	fn modify(&self) -> Result<kio::Mut<'_, TrackState>> {
		TrackState::modify(&self.state)
	}
}

/// Pop the next queued group fetch off the shared state and wrap it in a
/// [`GroupRequest`] bound to a fresh producer handle. Shared by every
/// [`TrackDynamic`] handle on the track.
fn poll_requested_group(state: &kio::Producer<TrackState>, waiter: &kio::Waiter) -> Poll<Result<GroupRequest>> {
	// Read-only predicate: ready once there's a request to pop, or the track aborted.
	let mut guard = ready!(state.poll(waiter, |state| {
		if state.fetches.is_empty() && state.abort.is_none() {
			Poll::Pending
		} else {
			Poll::Ready(())
		}
	}))
	.map_err(|state| state.abort.clone().unwrap_or(Error::Dropped))?;

	let req = match guard.fetches.pop_front() {
		Some(req) => req,
		// Woke because the track aborted while the fetch queue was empty.
		None => return Poll::Ready(Err(guard.abort.clone().unwrap_or(Error::Dropped))),
	};

	Poll::Ready(Ok(GroupRequest {
		state: state.clone(),
		sequence: req.sequence,
		priority: req.priority,
	}))
}

/// Serves on-demand fetches of uncached (old) groups for a track, the group-level
/// analogue of [`crate::BroadcastDynamic`].
///
/// Most tracks never serve old content, so this capability lives on a dedicated
/// handle rather than [`TrackProducer`]: a relay creates one (via
/// [`TrackProducer::dynamic`] or [`TrackRequest::dynamic`]) to pull past groups
/// from upstream. While at least one is alive the track will block a cache-miss
/// [`TrackConsumer::fetch_group`] waiting to be served; with none, an accepted track's
/// miss fails fast with [`Error::NotFound`].
pub struct TrackDynamic {
	name: Arc<str>,
	state: kio::Producer<TrackState>,
}

impl TrackDynamic {
	fn new(name: Arc<str>, state: kio::Producer<TrackState>) -> Self {
		if let Ok(mut state) = state.write() {
			state.dynamic += 1;
		}
		Self { name, state }
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	/// Block until a consumer fetches a group that isn't cached, returning a
	/// [`GroupRequest`] to serve via [`GroupRequest::accept`].
	///
	/// A relay issues a wire FETCH first; an origin already has the group cached, so
	/// the fetch resolves without ever reaching here. Errors once the track is aborted.
	pub async fn requested_group(&self) -> Result<GroupRequest> {
		kio::wait(|waiter| self.poll_requested_group(waiter)).await
	}

	pub fn poll_requested_group(&self, waiter: &kio::Waiter) -> Poll<Result<GroupRequest>> {
		poll_requested_group(&self.state, waiter)
	}

	/// Poll for the track becoming unused (every consumer dropped).
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}
}

impl Clone for TrackDynamic {
	fn clone(&self) -> Self {
		// Bump `dynamic` so each live handle is counted (mirrors `BroadcastDynamic`).
		if let Ok(mut state) = self.state.write() {
			state.dynamic += 1;
		}
		Self {
			name: self.name.clone(),
			state: self.state.clone(),
		}
	}
}

impl Drop for TrackDynamic {
	fn drop(&mut self) {
		// Unlike `BroadcastDynamic`, dropping the last handle doesn't abort the track:
		// a live `TrackProducer` may still be serving the subscription. It just stops
		// fetch serving, after which an accepted track's cache miss fails fast.
		if let Ok(mut state) = self.state.write() {
			state.dynamic = state.dynamic.saturating_sub(1);
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

/// Aggregate every live subscriber's preferences into the most demanding request.
///
/// Read-only: iterates `subscriptions` immutably and registers `waiter` on each, so it
/// never flags the [`TrackState`] as modified. Marking it modified would drain and wake
/// unrelated waiters on the channel (e.g. a [`TrackSubscribe`] parked on track info),
/// which races with [`TrackRequest::accept`] and can drop that wakeup. Callers decide
/// readiness from the returned value, then prune closed subscribers through the `Mut`.
fn combined_subscription(state: &TrackState, waiter: &kio::Waiter) -> Option<Subscription> {
	let mut combined = None;
	for sub in state.subscriptions.iter() {
		if let Poll::Ready(Ok(sub)) = sub.poll(waiter, |sub| sub.poll_combined(&combined)) {
			combined = Some(sub);
		}
	}
	combined
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

/// A cloneable, watch-only handle to a track's subscriber demand.
///
/// Obtained from [`TrackProducer::demand`]. A publisher uses it to react to
/// whether anyone is subscribed (on-demand capture / encoding) without being able
/// to publish frames or close the track. It's a weak handle, so it neither keeps
/// the track alive nor pins its cached groups; once the owning [`TrackProducer`]
/// goes away, [`used`](Self::used) / [`unused`](Self::unused) report the track's
/// closure.
#[derive(Clone)]
pub struct TrackDemand {
	name: Arc<str>,
	state: kio::Weak<TrackState>,
}

impl TrackDemand {
	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		self.state
			.used()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Block until the track is closed or aborted, returning the cause.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

/// A handle to a single track within a broadcast.
///
/// Obtained from [`crate::BroadcastConsumer::track`]. Holding it sends nothing
/// to the publisher; it just names a track you can [`subscribe`](Self::subscribe)
/// to (a live, ongoing stream of groups) later. The same handle can be subscribed
/// to multiple times, and clones are cheap.
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
	///
	/// Registers the subscription on the track and returns a [`kio::Pending`] that resolves to the
	/// [`TrackSubscriber`] once the track info is available, or the track's abort error (or
	/// [`Error::Dropped`]) if it is already closed.
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> kio::Pending<TrackSubscribe> {
		let subscription = kio::Producer::new(subscription.into().unwrap_or_default());

		// Register the subscription if the track is live. If it is already closed, the returned
		// future resolves to the abort error via `TrackSubscribe::poll_ok`.
		if let Ok(mut state) = self.state.write() {
			state.subscriptions.push(subscription.consume());
		}

		kio::Pending::new(TrackSubscribe {
			name: self.name.clone(),
			state: self.state.clone(),
			subscription,
		})
	}

	/// Return a cached group by sequence without blocking, or `None` if it isn't in
	/// the cache. Use [`Self::fetch_group`] to wait for a group that a [`TrackDynamic`]
	/// will serve on demand.
	pub fn get_group(&self, sequence: u64) -> Option<GroupConsumer> {
		self.state.read().cached_group(sequence)
	}

	/// Fetch a single past group, without holding a live subscription.
	///
	/// Returns a [`kio::Pending`] that resolves to the [`GroupConsumer`]:
	/// immediately if the group is cached, otherwise once a [`TrackDynamic`] serves
	/// the request (a wire FETCH for a relay). `options` accepts `None`, a [`Fetch`],
	/// or `Fetch::default()`.
	///
	/// The returned future resolves to [`Error::NotFound`] when the group can never be served
	/// (past the final sequence, or no [`TrackDynamic`] on the track), or the track's abort error
	/// if it's already closed.
	pub fn fetch_group(&self, sequence: u64, options: impl Into<Option<Fetch>>) -> kio::Pending<TrackFetch> {
		let options = options.into().unwrap_or_default();

		// Queue a request only when a handler can serve it but the group isn't cached yet. A cached
		// group, an unservable sequence (NotFound), or a closed track all resolve through
		// `TrackFetch::poll` without a queue entry.
		if let Ok(mut state) = self.state.write() {
			if state.poll_fetch(sequence).is_pending() {
				state.fetches.push_back(GroupRequested {
					sequence,
					priority: options.priority,
				});
			}
		}

		kio::Pending::new(TrackFetch {
			state: self.state.clone(),
			sequence,
		})
	}

	pub fn info(&self) -> kio::Pending<TrackInfoQuery> {
		kio::Pending::new(TrackInfoQuery {
			state: self.state.clone(),
		})
	}
}

/// The pollable state of a [`TrackConsumer::subscribe`]; awaited via the
/// [`kio::Pending`] wrapper, whose `DerefMut` exposes [`Self::update`].
pub struct TrackSubscribe {
	name: Arc<str>,
	state: kio::Consumer<TrackState>,
	subscription: kio::Producer<Subscription>,
}

impl TrackSubscribe {
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

	/// Change the subscription preferences before (or after) it resolves.
	pub fn update(&mut self, subscription: Subscription) {
		if let Ok(mut state) = self.subscription.write() {
			*state = subscription;
		} else {
			panic!("subscription is closed");
		}
	}
}

impl kio::Future for TrackSubscribe {
	type Output = Result<TrackSubscriber>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// The pollable state of a [`TrackConsumer::info`]; awaited via the
/// [`kio::Pending`] wrapper.
pub struct TrackInfoQuery {
	state: kio::Consumer<TrackState>,
}

impl TrackInfoQuery {
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<TrackInfo>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;
		Poll::Ready(Ok(info))
	}
}

impl kio::Future for TrackInfoQuery {
	type Output = Result<TrackInfo>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// A specific group requested via [`TrackConsumer::fetch_group`], queued on the
/// track for a [`TrackDynamic`] to serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GroupRequested {
	/// The group sequence the consumer wants.
	sequence: u64,
	/// The requested delivery priority.
	priority: u8,
}

/// A consumer's request for a single past group, handed to a handler via
/// [`TrackDynamic::requested_group`].
///
/// The handler fulfills it by calling [`Self::accept`], which inserts the group
/// into the track cache (resolving the matching [`TrackConsumer::fetch_group`]) and
/// returns a [`GroupProducer`] to fill. A relay typically opens a wire FETCH, reads
/// FETCH_OK, then accepts. The request carries its own producer handle, so it works
/// the same whether or not the track has been accepted yet.
pub struct GroupRequest {
	state: kio::Producer<TrackState>,
	sequence: u64,
	priority: u8,
}

impl GroupRequest {
	/// The group sequence the consumer wants.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// The delivery priority the consumer requested for this group.
	pub fn priority(&self) -> u8 {
		self.priority
	}

	/// Insert the fetched group into the track cache, resolving the waiting
	/// [`TrackConsumer::fetch_group`], and return a [`GroupProducer`] to fill.
	///
	/// The group's timescale comes from the track's [`TrackInfo`]. `info` sets that
	/// info if the track hasn't been accepted yet (a fetch with no live subscription),
	/// and is ignored once accepted. Returns [`Error::Duplicate`] if the group is
	/// already present, or the track's abort error if it closed while pending.
	pub fn accept(self, info: impl Into<Option<TrackInfo>>) -> Result<GroupProducer> {
		TrackState::modify(&self.state)?.insert_group_request(self.sequence, info.into())
	}
}

/// The pollable state of a [`TrackConsumer::fetch_group`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the
/// [`GroupConsumer`] once the group lands in the track's cache (already present,
/// or produced after a wire FETCH), or [`Error::NotFound`] if it can never exist.
pub struct TrackFetch {
	state: kio::Consumer<TrackState>,
	sequence: u64,
}

impl kio::Future for TrackFetch {
	type Output = Result<GroupConsumer>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		// `poll_fetch` already yields a `Result<GroupConsumer>` (group, or NotFound /
		// abort); the outer error is the channel closing without one.
		Poll::Ready(
			match ready!(self.state.poll(waiter, |state| state.poll_fetch(self.sequence))) {
				Ok(res) => res,
				Err(closed) => Err(closed.abort.clone().unwrap_or(Error::Dropped)),
			},
		)
	}
}

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

	// A requested track is served on demand, so it counts as fetch-capable from
	// birth: a consumer's cache-miss `fetch_group` waits to be served instead of
	// racing the producer (e.g. a relay) into creating its own handler. Released
	// when the request is accepted or dropped; by then the relay holds its own.
	_dynamic: TrackDynamic,
}

impl TrackRequest {
	pub fn new(name: impl Into<Arc<str>>) -> Self {
		let name = name.into();
		let state = kio::Producer::<TrackState>::default();
		let dynamic = TrackDynamic::new(name.clone(), state.clone());
		Self {
			name,
			state,
			prev_subscription: None,
			_dynamic: dynamic,
		}
	}

	/// The requested track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Attach a shared [`Cache`] to the track this request will become, so a cascaded
	/// broadcast/origin cache governs retention once the request is [`accept`](Self::accept)ed.
	/// The cache lives on the shared state, so it survives the request -> producer handoff.
	pub(crate) fn with_cache(self, cache: Cache) -> Self {
		if let Ok(mut state) = self.state.write() {
			state.cache = Some(cache);
		}
		self
	}

	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer {
			name: self.name.clone(),
			state: self.state.consume(),
		}
	}

	/// Create a [`TrackDynamic`] handle that serves on-demand fetches of uncached
	/// groups, before [`Self::accept`] is even called. A relay creates one to fetch
	/// past groups from upstream while (or instead of) serving a live subscription.
	pub fn dynamic(&self) -> TrackDynamic {
		TrackDynamic::new(self.name.clone(), self.state.clone())
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
		let mut combined = None;
		// The request owns the only producer, so the channel can't be closed here.
		let mut state = match ready!(self.state.poll(waiter, |state| {
			let next = combined_subscription(state, waiter);
			if &next == prev {
				Poll::Pending
			} else {
				combined = next;
				Poll::Ready(())
			}
		})) {
			Ok(state) => state,
			Err(_) => unreachable!("a TrackRequest holds the only producer"),
		};
		// The aggregate changed: prune any closed subscribers now that we hold the lock.
		state.subscriptions.retain(|sub| !sub.is_closed());
		drop(state);
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
		state.groups.iter().flatten().next().unwrap().group.sequence
	}

	/// A cache large enough to retain many small groups, with no age bound.
	fn unbounded_cache() -> Cache {
		Cache::new(cache::Config::default().with_max_bytes(u64::MAX))
	}

	#[tokio::test]
	async fn default_retains_recent_groups() {
		tokio::time::pause();
		let mut producer = TrackProducer::new("test", None);

		// The default 5-second cache keeps recently appended groups for late subscribers.
		producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1
		producer.append_group().unwrap(); // seq 2

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 3, "the default cache retains recent groups");
		assert!(state.duplicates.contains(&0));
		assert!(state.duplicates.contains(&2));
	}

	#[tokio::test]
	async fn default_evicts_after_window() {
		tokio::time::pause();
		let mut producer = TrackProducer::new("test", None);

		producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1 supersedes seq 0

		// Past the 5-second default window, the next append's retain pass evicts the aged group.
		tokio::time::advance(Duration::from_secs(6)).await;
		producer.append_group().unwrap(); // seq 2

		let state = producer.state.read();
		assert!(!state.duplicates.contains(&0), "group older than the window is evicted");
	}

	#[tokio::test]
	async fn zero_age_keeps_only_latest_group() {
		let mut producer = TrackProducer::new("test", None)
			.with_cache(Cache::new(cache::Config::default().with_max_age(Duration::ZERO)));

		// A zero-age cache is latest-only: each append supersedes the previous one immediately.
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
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());

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
		let mut producer =
			TrackProducer::new("test", None).with_cache(Cache::new(cache::Config::default().with_max_bytes(20)));

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

		let mut producer = TrackProducer::new("test", None).with_cache(Cache::new(
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

		let mut producer = TrackProducer::new("test", None).with_cache(Cache::new(
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
			assert!(consumer.get_group(0).is_some());
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
	async fn cache_access_via_read_frame_keeps_group_alive() {
		tokio::time::pause();

		let mut producer = TrackProducer::new("test", None).with_cache(Cache::new(
			cache::Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(5)),
		));

		// seq 0 is a single-frame group, superseded (and cached) by seq 1.
		producer.write_frame(b"hello".as_slice()).unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1

		// Read seq 0's frame within each window so the frame-level read keeps it recent.
		for _ in 0..4 {
			tokio::time::advance(Duration::from_secs(2)).await;
			let mut sub = producer.subscribe(None);
			sub.end_at(0);
			let frame = sub.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(&frame[..], b"hello");
			producer.append_group().unwrap();
		}

		assert!(
			producer.state.read().duplicates.contains(&0),
			"a group kept alive by frame reads is not aged out"
		);
	}

	#[tokio::test]
	async fn cache_aged_group_evicted_on_read_not_revived() {
		tokio::time::pause();

		let mut producer = TrackProducer::new("test", None).with_cache(Cache::new(
			cache::Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(5)),
		));

		let g0 = producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap(); // seq 1, supersedes (and caches) seq 0

		let consumer = producer.consume();

		// Let seq 0 age out, then read it: the read must evict it, not revive it.
		tokio::time::advance(Duration::from_secs(6)).await;
		assert!(consumer.get_group(0).is_none(), "an aged-out group reads as a miss");
		assert!(
			g0.is_aborted(),
			"the stale group is evicted (aborted) on read, not refreshed"
		);
	}

	#[tokio::test]
	async fn shared_cache_one_budget_across_tracks() {
		let cache = Cache::new(cache::Config::default().with_max_bytes(20));

		let mut track_a = TrackProducer::new("a", None).with_cache(cache.clone());
		let mut track_b = TrackProducer::new("b", None).with_cache(cache.clone());

		assert!(cache.is_clone(&cache.clone()), "clones share one budget");

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
	async fn cache_grown_group_counted_at_current_size() {
		// A superseded group can still receive late frames and grow; eviction must count its
		// current size, not the snapshot captured when it was first cached.
		let mut producer =
			TrackProducer::new("test", None).with_cache(Cache::new(cache::Config::default().with_max_bytes(25)));

		// seq 0 starts at 10 bytes, then gets superseded and cached.
		let mut g0 = producer.create_group(Group { sequence: 0 }).unwrap();
		g0.write_frame(bytes::Bytes::from(vec![0u8; 10])).unwrap();
		producer.create_group(Group { sequence: 1 }).unwrap(); // latest, uncounted

		assert!(producer.state.read().duplicates.contains(&0), "seq 0 fits at 10 bytes");

		// A late frame grows seq 0 from 10 to 30 bytes (over the 25-byte budget).
		g0.write_frame(bytes::Bytes::from(vec![0u8; 20])).unwrap();

		// Append another group to run an eviction pass; seq 0 is now over budget at its grown
		// size and is evicted.
		producer.create_group(Group { sequence: 2 }).unwrap();
		assert!(
			!producer.state.read().duplicates.contains(&0),
			"a grown superseded group is evicted at its current size"
		);
	}

	#[tokio::test]
	async fn cache_never_evicts_max_sequence() {
		// A budget of 0 still keeps the latest group: it is never handed to the cache.
		let mut producer =
			TrackProducer::new("test", None).with_cache(Cache::new(cache::Config::default().with_max_bytes(0)));

		let mut g = producer.append_group().unwrap();
		g.write_frame(bytes::Bytes::from(vec![0u8; 1024])).unwrap();

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 1, "the latest group survives a zero budget");
		assert_eq!(first_live_sequence(&state), 0);
	}

	/// A latest-group-only cache: zero retention window, so every superseded group is dropped.
	fn latest_only_cache() -> Cache {
		Cache::new(cache::Config::default().with_max_age(Duration::ZERO))
	}

	#[tokio::test]
	async fn consumer_skips_evicted_groups() {
		let mut producer = TrackProducer::new("test", None).with_cache(latest_only_cache());
		producer.append_group().unwrap(); // seq 0

		let mut consumer = producer.subscribe(None);

		producer.append_group().unwrap(); // seq 1, supersedes (and drops) seq 0

		// Group 0 was evicted (latest-only). Consumer should get group 1.
		let group = consumer.assert_group();
		assert_eq!(group.sequence, 1);
	}

	#[tokio::test]
	async fn out_of_order_max_sequence_at_front() {
		let mut producer = TrackProducer::new("test", None).with_cache(latest_only_cache());

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
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());

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
		let mut consumer = producer.subscribe(None);
		assert_eq!(consumer.assert_group().sequence, 5);
	}

	#[tokio::test]
	async fn abort_clears_cached_groups() {
		// A cache retains both groups so we can verify abort drops them and releases the budget.
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
		producer.append_group().unwrap();
		producer.append_group().unwrap();

		// A stale consumer that never drains must not pin the cached groups.
		let mut consumer = producer.subscribe(None);
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

	#[test]
	fn stale_clamped_to_cache_max_age() {
		// The stale window is clamped to the attached cache's max_age: a subscriber can't wait
		// for a late group longer than the cache keeps it.
		let cache = Cache::new(
			cache::Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(2)),
		);
		let producer = TrackProducer::new("test", None).with_cache(cache);

		let mut subscriber = producer.subscribe(Subscription::default().with_stale(Duration::from_secs(10)));
		assert_eq!(subscriber.subscription().stale, Duration::from_secs(2));

		// A window within max_age is left alone, and ZERO (skip immediately) stays ZERO.
		subscriber.update(Subscription::default().with_stale(Duration::from_millis(500)));
		assert_eq!(subscriber.subscription().stale, Duration::from_millis(500));

		subscriber.update(Subscription::default().with_stale(Duration::ZERO));
		assert_eq!(subscriber.subscription().stale, Duration::ZERO);
	}

	#[test]
	fn stale_clamped_to_default_window() {
		// A bare track carries the 5-second default cache, so a long stale window clamps to it.
		let producer = TrackProducer::new("test", None);
		let subscriber = producer.subscribe(Subscription::default().with_stale(Duration::from_secs(10)));
		assert_eq!(subscriber.subscription().stale, DEFAULT_CACHE);
	}

	#[test]
	fn stale_clamped_to_zero_for_latest_only() {
		// A zero-age (latest-only) cache keeps nothing beyond the current group, so a stale window
		// is pointless and collapses to ZERO.
		let producer = TrackProducer::new("test", None).with_cache(latest_only_cache());
		let subscriber = producer.subscribe(Subscription::default().with_stale(Duration::from_secs(10)));
		assert_eq!(subscriber.subscription().stale, Duration::ZERO);
	}

	#[tokio::test]
	async fn drop_unfinished_clears_cached_groups() {
		let producer = TrackProducer::new("test", None);
		let mut writer = producer.clone();
		writer.append_group().unwrap();

		// A stale consumer keeps the channel (and thus the cache) alive.
		let mut consumer = producer.subscribe(None);
		assert_eq!(live_groups(&producer.state.read()), 1);

		// Drop every producer without finishing: the cache is released.
		drop(writer);
		drop(producer);

		let result = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn drop_finished_keeps_cached_groups() {
		let mut producer = TrackProducer::new("test", None);
		producer.append_group().unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.subscribe(None);
		drop(producer);

		// A cleanly finished track keeps its cache so the consumer can still drain.
		assert_eq!(consumer.assert_group().sequence, 0);
		let done = consumer.recv_group().now_or_never().expect("should not block").unwrap();
		assert!(done.is_none(), "consumer should drain then see clean finish");
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
		// A cache keeps both groups retained so the consumer sees the full arrival order.
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		// A cache retains the late seq 3 behind the protected latest seq 5.
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		// A cache retains both single-frame groups so the consumer can read each in turn.
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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
		// A cache retains group 0 so the consumer reads its first frame before group 1.
		let mut producer = TrackProducer::new("test", None).with_cache(unbounded_cache());
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

		// A cached group resolves immediately and never queues a request. `get_group`
		// also returns it synchronously.
		let dynamic = producer.dynamic();
		let consumer = producer.consume();
		assert!(consumer.get_group(0).is_some());
		let mut g = consumer.fetch_group(0, None).await.unwrap();
		assert_eq!(g.sequence, 0);
		assert_eq!(&g.read_frame().await.unwrap().unwrap()[..], b"hello");

		// Nothing was queued for the dynamic handler to serve.
		assert!(dynamic.poll_requested_group(&kio::Waiter::noop()).is_pending());
	}

	#[tokio::test]
	async fn fetch_miss_signals_dynamic() {
		let producer = TrackProducer::new("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		// A cache miss isn't in `get_group`, but a dynamic handler exists, so
		// `fetch_group` stays pending and queues a request. `*pending` derefs the
		// wrapper to the inner `TrackFetch` (a `kio::Future`).
		assert!(consumer.get_group(5).is_none());
		let pending = consumer.fetch_group(5, Fetch::default().with_priority(7));
		assert!(kio::Future::poll(&*pending, &kio::Waiter::noop()).is_pending());

		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
		assert_eq!(req.priority(), 7);

		// Serve it by accepting the request; the fetch then resolves.
		let mut group = req.accept(None).unwrap();
		group.write_frame(bytes::Bytes::from_static(b"hi")).unwrap();
		group.finish().unwrap();

		let mut g = pending.await.unwrap();
		assert_eq!(g.sequence, 5);
		assert_eq!(&g.read_frame().await.unwrap().unwrap()[..], b"hi");
	}

	#[tokio::test]
	async fn fetch_miss_no_dynamic_not_found() {
		// A track with no `TrackDynamic` can't serve old content, so a cache miss
		// resolves to NotFound instead of blocking forever.
		let mut producer = TrackProducer::new("test", None);
		producer.append_group().unwrap(); // seq 0, but we miss on seq 5
		let consumer = producer.consume();
		assert!(matches!(consumer.fetch_group(5, None).await, Err(Error::NotFound)));
	}

	#[tokio::test]
	async fn fetch_past_final_not_found() {
		let mut producer = TrackProducer::new("test", None);
		producer.append_group().unwrap(); // seq 0
		producer.finish().unwrap(); // final_sequence = 1

		// A group at or past the final sequence can never exist, even with a handler,
		// so it resolves to NotFound.
		let dynamic = producer.dynamic();
		let consumer = producer.consume();
		assert!(matches!(consumer.fetch_group(5, None).await, Err(Error::NotFound)));

		// And it doesn't signal the dynamic handler.
		assert!(dynamic.poll_requested_group(&kio::Waiter::noop()).is_pending());
	}

	#[tokio::test]
	async fn fetch_aborts_with_track() {
		let mut producer = TrackProducer::new("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(3, None);
		assert!(kio::Future::poll(&*pending, &kio::Waiter::noop()).is_pending());

		producer.abort(Error::Cancel).unwrap();
		assert!(pending.await.is_err());
		drop(dynamic);
	}
}
