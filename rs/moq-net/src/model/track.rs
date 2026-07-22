//! A track is a collection of semi-reliable and semi-ordered streams, split into a [Producer] and [Subscriber] handle.
//!
//! A [Producer] creates streams with a sequence number and priority.
//! The sequence number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [Producer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [Subscriber] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Transport is to not block on them.
//! Streams will be cached for a potentially limited duration added to the unreliable nature.
//! A [Consumer] is a cheap, cloneable handle; subscribing it multiple times fans the same
//! cached streams out to each independent [Subscriber].
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use crate::{Error, Result, Timescale, Timestamp, coding};
use crate::{broadcast, cache, frame, group, stats};

use super::{Datagram, Requests};

pub use super::subscription::Subscription;

use std::{
	collections::{HashSet, VecDeque},
	sync::Arc,
	task::{Poll, ready},
	time::Duration,
};

/// Default [`Info::latency_max`] age when the publisher doesn't set one.
pub const DEFAULT_LATENCY_MAX: Duration = Duration::from_secs(5);

/// How long a datagram stays in the per-track buffer before it is dropped.
///
/// Datagrams are a best-effort send buffer, not a replay cache (unlike groups): only the last
/// few tens of milliseconds are kept, so a consumer that stalls loses stale datagrams instead of
/// replaying them. Sized like a typical send buffer for real-time audio/video.
const MAX_DATAGRAM_AGE: Duration = Duration::from_millis(50);

/// Publisher-side properties of a track.
///
/// These are fixed by the publisher when the track is created and don't change
/// while the track is alive. A subscriber learns them via
/// [`broadcast::Consumer::track`](broadcast::Consumer::track),
/// which returns the publisher's [`Info`] once the subscription is accepted.
///
/// Not `Copy`: it carries an internal handle to its parent broadcast.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Info {
	/// Units per second for per-frame timestamps on this track.
	///
	/// Every track is timed; this defaults to [`Timescale::MILLI`]. On Lite05+ it is
	/// reported in TRACK_INFO and the publisher zigzag-delta encodes per-frame
	/// timestamps at this scale on the wire. Protocols whose wire can't carry it
	/// (pre-Lite05 moq-lite, IETF moq-transport) fall back to local monotonic milliseconds.
	pub timescale: Timescale,
	/// The maximum age of a non-latest group before the publisher evicts it (the
	/// newest group is always retained). A subscriber's
	/// [`Subscription::latency_max`] window is clamped to this, since a group can't be
	/// waited for longer than it's kept around. Reported in TRACK_INFO so
	/// relays re-serve with the same window. Defaults to [`DEFAULT_LATENCY_MAX`].
	///
	/// This is the `Publisher Max Latency` on the wire, the publisher-side half of
	/// the same budget [`Subscription::latency_max`] sets for a subscriber.
	pub latency_max: Duration,
	/// The publisher's priority for this track, used only to break ties between
	/// subscriptions of equal subscriber priority. Reported in TRACK_INFO (Lite05+).
	pub priority: u8,
	/// Whether groups are prioritized in sequence order. Groups may always arrive
	/// out-of-order (or not at all) over the network. Used only to break ties,
	/// reported in TRACK_INFO (Lite05+), and defaults to `false` (newest-first).
	pub ordered: bool,

	// The broadcast this track belongs to, bound when the track is created under one
	// (`create_track` / `reserve_track` / `Request::accept`); until then it's a
	// standalone broadcast with an unbounded pool. Not on the wire, and not a knob:
	// every bind path overwrites whatever is here. It's the parent link a group walks
	// to reach the shared cache pool (`track.broadcast.origin.pool`), which stays
	// crate-private.
	pub(crate) broadcast: Arc<broadcast::Info>,
}

/// The shared parent for a not-yet-bound [`Info`]: a standalone broadcast with an
/// unbounded pool. Cheap to hand out (one `Arc` clone) and replaced the moment the
/// track is bound to a real broadcast.
fn default_broadcast() -> Arc<broadcast::Info> {
	static DEFAULT: std::sync::LazyLock<Arc<broadcast::Info>> =
		std::sync::LazyLock::new(|| Arc::new(broadcast::Info::default()));
	DEFAULT.clone()
}

impl Default for Info {
	fn default() -> Self {
		Self {
			timescale: Timescale::default(),
			latency_max: DEFAULT_LATENCY_MAX,
			priority: 0,
			ordered: false,
			broadcast: default_broadcast(),
		}
	}
}

impl Info {
	/// Set the per-frame timestamp scale, returning `self` for chaining.
	///
	/// Defaults to [`Timescale::MILLI`]. On Lite05+ this scale is reported in TRACK_INFO
	/// and used to encode per-frame timestamps on the wire.
	pub fn with_timescale(mut self, timescale: Timescale) -> Self {
		self.timescale = timescale;
		self
	}

	/// Set the maximum age of a non-latest group before eviction, returning `self` for chaining.
	pub fn with_latency_max(mut self, latency_max: Duration) -> Self {
		self.latency_max = latency_max;
		self
	}

	/// Set the publisher's tie-break priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: u8) -> Self {
		self.priority = priority;
		self
	}

	/// Set whether groups are prioritized in sequence order, returning `self` for
	/// chaining. Groups may always arrive out-of-order (or not at all) over the
	/// network. Defaults to `false`.
	pub fn with_ordered(mut self, ordered: bool) -> Self {
		self.ordered = ordered;
		self
	}
}

#[derive(Default)]
struct TrackState {
	// The info for the track; always Some for Subscriber/Producer. Inherited (cloned)
	// by each group it creates, which reaches the cache pool through its `broadcast`.
	info: Option<Info>,

	// The broadcast this track belongs to, the source for stamping `Info::broadcast`
	// on groups (and on a not-yet-accepted track's default info). Its
	// `origin.pool` is the shared cache pool every group registers with.
	broadcast: Arc<broadcast::Info>,

	// The pool registration of the current max_sequence group, pinned so the
	// latest group is immune to pool eviction. `None` when the pool is detached
	// or no group exists yet.
	latest_entry: Option<Arc<cache::Entry>>,

	// Groups in arrival order. `None` entries are tombstones for evicted groups.
	groups: VecDeque<Option<(group::Producer, web_async::time::Instant)>>,

	// Datagrams in arrival order paired with their arrival time, a best-effort send buffer
	// evicted by age (see `MAX_DATAGRAM_AGE`). Shares the group `max_sequence` namespace but
	// is otherwise independent.
	datagrams: VecDeque<(Datagram, web_async::time::Instant)>,

	// Number of datagrams dropped off the front (aged out), mapping a subscriber's absolute
	// cursor to an index into `datagrams` (mirrors `offset` for groups).
	datagram_offset: usize,

	// Sequences currently occupying a cache slot, used to reject a duplicate
	// `create_group`/`write_group` with `Error::Duplicate`. Entries are removed on
	// expiry/eviction so a pool-evicted sequence can be re-fetched into its slot.
	duplicates: HashSet<u64>,

	// We've popped the front of this VecDeque this many times, used to map sequence -> index.
	offset: usize,

	// The highest sequence number successfully appended to the track.
	max_sequence: Option<u64>,

	// The sequence number at which the track was finalized.
	final_sequence: Option<u64>,

	// The error that caused the track to be aborted, if any.
	abort: Option<Error>,

	// Active subscriptions, in their own [`kio::Shared`] so a read-only `Consumer`
	// registers under that lock instead of writing back into the track state.
	// Kept here (rather than threaded through every handle) so any holder reaches it.
	subscriptions: kio::Shared<Subscriptions>,

	// The reverse fetch queue (see [`FetchState`]), same reasoning: cache-miss
	// `fetch_group` calls enqueue here and a `Dynamic` drains.
	fetch: kio::Shared<FetchState>,
}

/// The registered subscriptions, aggregated by the producer.
type Subscriptions = Vec<kio::Consumer<Subscription>>;

/// Reverse state for [`Consumer::fetch_group`], beside the track state in its own
/// [`kio::Shared`]: consumers enqueue (coalescing per sequence, so a relay opens one
/// upstream FETCH per group) and [`Dynamic`] handlers drain under one lock, without
/// write access to the track itself.
type FetchState = Requests<u64, PendingFetch>;

/// One fetch attempt for a sequence, shared by every [`Fetching`] that joined it.
struct PendingFetch {
	// The most demanding delivery priority across the joined fetches.
	priority: u8,

	// Result channel back to the joined fetches. Written only on rejection; a
	// successful accept resolves them through the track cache instead. Dropping
	// every producer without writing (a vanished handler) closes the channel,
	// which a [`Fetching`] reads as [`Error::NotFound`].
	result: kio::Producer<FetchOutcome>,
}

/// The result of a fetch attempt. Stays empty on success (the group lands in the
/// track cache); a handler writes `rejected` to fail every joined fetch.
#[derive(Default)]
struct FetchOutcome {
	rejected: Option<Error>,
}

impl TrackState {
	fn poll_info(&self) -> Poll<Result<Info>> {
		if let Some(info) = &self.info {
			Poll::Ready(Ok(info.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Find the next non-tombstoned group at or after `index` in arrival order.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	fn poll_recv_group(&self, index: usize, min_sequence: u64) -> Poll<Result<Option<(group::Consumer, usize)>>> {
		let start = index.saturating_sub(self.offset);
		for (i, slot) in self.groups.iter().enumerate().skip(start) {
			if let Some((group, _)) = slot
				&& group.sequence >= min_sequence
				&& !group.is_aborted()
			{
				return Poll::Ready(Ok(Some((group.consume(), self.offset + i))));
			}
		}

		// TODO once we have drop notifications, check if index == final_sequence.
		if self.is_complete() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Find the next datagram at or after the subscriber's absolute `index`.
	///
	/// Returns the datagram and its absolute index so the consumer can advance past it. A
	/// consumer whose `index` has fallen behind `datagram_offset` (older datagrams dropped)
	/// resumes at the oldest still-buffered datagram, skipping the lost ones.
	fn poll_recv_datagram(&self, index: usize) -> Poll<Result<Option<(Datagram, usize)>>> {
		let start = index.saturating_sub(self.datagram_offset);
		if let Some((datagram, _)) = self.datagrams.get(start) {
			return Poll::Ready(Ok(Some((datagram.clone(), self.datagram_offset + start))));
		}

		// Nothing buffered at the cursor: the track ending terminates the datagram stream too.
		if self.is_complete() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Push a datagram onto the buffer, dropping any that have aged past [`MAX_DATAGRAM_AGE`].
	fn push_datagram(&mut self, datagram: Datagram) {
		let now = web_async::time::Instant::now();
		self.datagrams.push_back((datagram, now));
		while let Some((_, at)) = self.datagrams.front() {
			if now.duration_since(*at) <= MAX_DATAGRAM_AGE {
				break;
			}
			self.datagrams.pop_front();
			self.datagram_offset += 1;
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
	) -> Poll<Result<Option<(frame::Frame, usize, u64)>>> {
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
				// A single group failing (aborted upstream, or evicted from the
				// cache) doesn't poison the track; skip it like a gap.
				Poll::Ready(Err(_)) => continue,
				Poll::Pending => {
					pending_seen = true;
					continue;
				}
			}
		}

		// A pending group can still produce a frame even after finish(). Finish only
		// blocks new groups at/above final_sequence, not frames on existing groups.
		if pending_seen {
			Poll::Pending
		} else if self.is_complete() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Find the smallest-sequence cached group satisfying
	/// `next_sequence <= seq <= end_sequence (if set)`. Used by
	/// [`Subscriber::next_group`] so the range can be widened (or unset)
	/// after the fact and previously-skipped cached groups become available
	/// without scanning past them in arrival order.
	///
	/// Returns `Poll::Pending` when no in-range group is currently cached but
	/// future groups could still arrive in range; returns `Ok(None)` only when
	/// the track is finalized and no further in-range group is possible.
	fn poll_next_in_range(
		&self,
		next_sequence: u64,
		end_sequence: Option<u64>,
	) -> Poll<Result<Option<group::Consumer>>> {
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

		let mut best: Option<&group::Producer> = None;
		for (group, _) in self.groups.iter().flatten() {
			if group.sequence < next_sequence {
				continue;
			}
			if let Some(end) = end_sequence
				&& group.sequence > end
			{
				continue;
			}
			if group.is_aborted() {
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

	/// Find a cached group by sequence, skipping tombstones and groups evicted from
	/// the cache pool (a fetch treats those as a miss and re-fetches). Synchronous,
	/// never blocks.
	fn cached_group(&self, sequence: u64) -> Option<group::Consumer> {
		self.groups
			.iter()
			.flatten()
			.find(|(group, _)| group.sequence == sequence && !group.is_aborted())
			.map(|(group, _)| group.consume())
	}

	/// The publisher's latency window, or `None` while the info is unknown (an
	/// unaccepted [`Request`]). Bounds the aggregate subscription; see [`clamp_combined`].
	fn latency_bound(&self) -> Option<Duration> {
		self.info.as_ref().map(|info| info.latency_max)
	}

	/// Resolve a one-shot fetch from the track side: the cached group, or an [`Error`]
	/// once it can never be served. A missing group is a failure ([`Error::NotFound`]), not an
	/// end-of-stream. The handler side (a rejection, or no [`Dynamic`] at all) lives
	/// in [`FetchState`]; [`Fetching`] polls both.
	fn poll_fetch_cached(&self, sequence: u64) -> Poll<Result<group::Consumer>> {
		if let Some(group) = self.cached_group(sequence) {
			return Poll::Ready(Ok(group));
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}

		// Past the final sequence: the group can never exist.
		if self.final_sequence.is_some_and(|fin| sequence >= fin) {
			return Poll::Ready(Err(Error::NotFound));
		}

		Poll::Pending
	}

	/// Evict groups older than `max_age`, never evicting the max_sequence group.
	///
	/// Groups are in arrival order, so we can stop early when we hit a non-expired,
	/// non-max_sequence group (everything after it arrived even later).
	/// When max_sequence is at the front, we skip past it and tombstone expired groups
	/// behind it.
	///
	/// Also reaps slots whose group the cache pool already evicted (walked before
	/// the early exit); ones behind fresh groups stay as soft tombstones that every
	/// read path skips, and are replaced in place if the sequence is re-fetched.
	fn evict_expired(&mut self, now: web_async::time::Instant, max_age: Duration) {
		for slot in self.groups.iter_mut() {
			let Some((group, created_at)) = slot else { continue };

			// Evicted by the pool: the frames are already gone, reclaim the slot
			// and the sequence so a later fetch can re-insert it.
			if group.is_aborted() {
				self.duplicates.remove(&group.sequence);
				*slot = None;
				continue;
			}

			if Some(group.sequence) == self.max_sequence {
				continue;
			}

			if now.duration_since(*created_at) <= max_age {
				break;
			}

			self.duplicates.remove(&group.sequence);
			// Abort the group before dropping it so any consumer still reading it
			// surfaces `Error::Old` instead of blocking forever on a frame that will
			// never arrive (the cached producer is about to be gone). Without this a
			// reader parked on an aged-out group hangs indefinitely, since the group
			// was never finished or aborted -- it just silently disappeared.
			let _ = group.abort(Error::Old);
			*slot = None;
		}

		// Trim leading tombstones to advance the offset.
		while let Some(None) = self.groups.front() {
			self.groups.pop_front();
			self.offset += 1;
		}
	}

	/// Pin `group` as the latest (immune to pool eviction) if it holds the track's
	/// max_sequence, releasing the previous pin. Call after updating `max_sequence`.
	fn pin_latest(&mut self, group: &group::Producer) {
		if Some(group.sequence) != self.max_sequence {
			return;
		}
		if let Some(prev) = self.latest_entry.take() {
			prev.set_pinned(false);
		}
		if let Some(entry) = group.cache_entry() {
			entry.set_pinned(true);
			self.latest_entry = Some(entry);
		}
	}

	/// Record the exclusive final sequence, rejecting a re-finish or a boundary that
	/// would orphan already-produced groups.
	fn set_final(&mut self, final_sequence: u64) -> Result<()> {
		if self.final_sequence.is_some() {
			return Err(Error::Closed);
		}
		if let Some(max) = self.max_sequence
			&& final_sequence <= max
		{
			return Err(Error::ProtocolViolation);
		}
		self.final_sequence = Some(final_sequence);
		Ok(())
	}

	/// Whether the track has reached its end: the final boundary is set and the live
	/// edge has caught up to it, so no further group can arrive. A future boundary
	/// (declared via [`Producer::finish_at`] ahead of the live edge) stays incomplete
	/// until the remaining groups are produced. Drives the end-of-stream signal from
	/// the read methods (`recv_group` / `next_group` / `read_frame` return `None`).
	fn is_complete(&self) -> bool {
		self.final_sequence
			.is_some_and(|fin| self.max_sequence.map_or(0, |max| max.saturating_add(1)) >= fin)
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

	/// Replace the slot of a duplicate `sequence` whose group was evicted from the
	/// cache pool, returning the fresh producer. `Err(Duplicate)` when the cached
	/// group is still live; `None` when no slot holds the sequence.
	fn replace_evicted(
		&mut self,
		sequence: u64,
		track: Info,
		now: web_async::time::Instant,
	) -> Option<Result<group::Producer>> {
		let slot = self
			.groups
			.iter_mut()
			.find(|slot| matches!(slot, Some((group, _)) if group.sequence == sequence))?;
		let (existing, _) = slot.as_ref().unwrap();
		if !existing.is_aborted() {
			return Some(Err(Error::Duplicate));
		}
		let group = group::Producer::new(group::Info { sequence }, track);
		*slot = Some((group.clone(), now));
		// The replaced group can hold max_sequence when the publisher aborted the
		// latest group itself; re-pin so the live edge stays eviction-immune.
		self.pin_latest(&group);
		Some(Ok(group))
	}

	/// Insert a group fetched for a [`GroupRequest`], setting the track's [`Info`]
	/// if it isn't accepted yet. The group's timescale comes from that info, so a
	/// fetch can serve an as-yet-unaccepted track (e.g. a relay with no live
	/// subscription). The group lands in the cache so a waiting
	/// [`Fetching`] resolves via [`Self::poll_fetch`].
	fn insert_group_request(&mut self, sequence: u64, info: Option<Info>) -> Result<group::Producer> {
		if let Some(err) = &self.abort {
			return Err(err.clone());
		}
		if let Some(fin) = self.final_sequence
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}

		// Adopt the supplied info only if the track hasn't been accepted yet, binding
		// it to this track's broadcast so its groups reach the shared pool.
		let now = web_async::time::Instant::now();
		let broadcast = self.broadcast.clone();
		let info = self
			.info
			.get_or_insert_with(|| {
				let mut info = info.unwrap_or_default();
				info.broadcast = broadcast;
				info
			})
			.clone();

		if !self.duplicates.insert(sequence) {
			// A pool-evicted group can be re-fetched into its old slot.
			return self
				.replace_evicted(sequence, info, now)
				.unwrap_or(Err(Error::Duplicate));
		}

		let latency_max = info.latency_max;
		let group = group::Producer::new(group::Info { sequence }, info);
		self.max_sequence = Some(self.max_sequence.unwrap_or(0).max(sequence));
		self.groups.push_back(Some((group.clone(), now)));
		self.pin_latest(&group);
		self.evict_expired(now, latency_max);
		Ok(group)
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct Producer {
	name: Arc<str>,
	// The parent broadcast's info, inherited from [`broadcast::Producer::create_track`].
	// Top link of the ownership chain; carried for identity and future inheritance.
	broadcast: Arc<broadcast::Info>,
	state: kio::Producer<TrackState>,
	prev_subscription: Option<Subscription>,
	// Ingress stats scope, inherited from a tagged [`broadcast::Producer`]. Bumped as
	// one subscription on tag and closed when the last producer clone drops. Empty
	// (no-op) for an untagged broadcast.
	stats: stats::Scope,
}

impl Producer {
	/// Build a producer for the given track metadata.
	///
	/// Crate-private: tracks are born from their broadcast via
	/// [`broadcast::Producer::create_track`] (or served on demand through a
	/// [`Request`]), which threads the broadcast's `Arc<broadcast::Info>` down. The
	/// track binds it onto its [`Info`] so every group reaches the shared cache pool
	/// by walking `track.broadcast.origin.pool`.
	pub(crate) fn new(
		broadcast: Arc<broadcast::Info>,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<Info>>,
	) -> Self {
		let mut info = info.into().unwrap_or_default();
		info.broadcast = broadcast.clone();
		Self {
			name: name.into(),
			state: kio::Producer::new(TrackState {
				info: Some(info),
				broadcast: broadcast.clone(),
				..Default::default()
			}),
			broadcast,
			prev_subscription: None,
			stats: stats::Scope::default(),
		}
	}

	/// Attach the parent broadcast's ingress stats scope, counting this track as one
	/// ingress subscription (closed when the last producer clone drops). Called by a
	/// tagged [`broadcast::Producer`] when it creates the track.
	pub(crate) fn with_stats(mut self, scope: stats::Scope) -> Self {
		scope.open_subscription();
		self.stats = scope;
		self
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The parent broadcast this track belongs to.
	pub fn broadcast(&self) -> &broadcast::Info {
		&self.broadcast
	}

	/// Create a new group with the given sequence number.
	pub fn create_group(&mut self, group: group::Info) -> Result<group::Producer> {
		let mut state = self.modify()?;
		if let Some(fin) = state.final_sequence
			&& group.sequence >= fin
		{
			return Err(Error::Closed);
		}
		let info = state.info.as_ref().unwrap();
		let track = info.clone();
		let latency_max = info.latency_max;
		let now = web_async::time::Instant::now();

		if !state.duplicates.insert(group.sequence) {
			// A pool-evicted group can be re-created into its old slot.
			return state
				.replace_evicted(group.sequence, track, now)
				.unwrap_or(Err(Error::Duplicate));
		}

		let group = group::Producer::new(group, track).with_meter(self.stats.meter());
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.sequence));
		state.groups.push_back(Some((group.clone(), now)));
		state.pin_latest(&group);
		state.evict_expired(now, latency_max);

		Ok(group)
	}

	/// Create a new group with the next sequence number.
	pub fn append_group(&mut self) -> Result<group::Producer> {
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
		let track = info.clone();
		let latency_max = info.latency_max;

		let group = group::Producer::new(group::Info { sequence }, track).with_meter(self.stats.meter());

		let now = web_async::time::Instant::now();
		state.duplicates.insert(sequence);
		state.max_sequence = Some(sequence);
		state.groups.push_back(Some((group.clone(), now)));
		state.pin_latest(&group);
		state.evict_expired(now, latency_max);

		Ok(group)
	}

	/// Append a datagram with the next sequence number, returning the assigned sequence.
	///
	/// A datagram is delivered best-effort over a single QUIC datagram, parallel to the
	/// track's groups but drawing from the same sequence namespace (so interleaving with
	/// [`Self::append_group`] never reuses a number). There is no group fallback: each
	/// session drops (with a debug log) any datagram whose encoded body exceeds the
	/// transport's datagram size, and sessions that can't carry datagrams at all (IETF
	/// moq-transport, moq-lite before 05, or stream-only transports like WebSocket) never
	/// deliver them. Keep payloads well under the 1200-byte minimum path MTU. An origin
	/// publisher uses this; a relay preserving upstream numbering uses
	/// [`Self::write_datagram`].
	pub fn append_datagram<B: crate::IntoBytes>(&mut self, timestamp: Timestamp, payload: B) -> Result<u64> {
		let payload = payload.into_bytes();
		if payload.len() > super::datagram::MAX_DATAGRAM_PAYLOAD {
			return Err(Error::FrameTooLarge);
		}
		// Resolved before the state guard borrows `self`.
		let meter = self.stats.meter();
		let mut state = self.modify()?;
		// Normalize into the track's timescale, like frames (see `group::Producer::create_frame`).
		let timescale = state.info.as_ref().unwrap().timescale;
		let timestamp = timestamp.convert(timescale).map_err(|_| Error::TimestampMismatch)?;
		let sequence = match state.max_sequence {
			Some(s) => s.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		};
		if let Some(fin) = state.final_sequence
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}
		state.max_sequence = Some(sequence);
		meter.datagram(payload.len() as u64);
		state.push_datagram(Datagram {
			sequence,
			timestamp,
			payload,
		});
		Ok(sequence)
	}

	/// Write a datagram with an explicit sequence number.
	///
	/// Preserves the supplied sequence (bumping the shared `max_sequence` if needed), so a
	/// relay can forward a datagram without renumbering it. Most origin publishers want
	/// [`Self::append_datagram`] instead.
	pub fn write_datagram(&mut self, mut datagram: Datagram) -> Result<()> {
		if datagram.payload.len() > super::datagram::MAX_DATAGRAM_PAYLOAD {
			return Err(Error::FrameTooLarge);
		}
		// Resolved before the state guard borrows `self`.
		let meter = self.stats.meter();
		let mut state = self.modify()?;
		// Normalize into the track's timescale, like frames (see `group::Producer::create_frame`).
		let timescale = state.info.as_ref().unwrap().timescale;
		datagram.timestamp = datagram
			.timestamp
			.convert(timescale)
			.map_err(|_| Error::TimestampMismatch)?;
		if let Some(fin) = state.final_sequence
			&& datagram.sequence >= fin
		{
			return Err(Error::Closed);
		}
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(datagram.sequence));
		meter.datagram(datagram.payload.len() as u64);
		state.push_datagram(datagram);
		Ok(())
	}

	/// Create a group with a single frame, at the given presentation timestamp.
	///
	/// The timestamp is converted into the track's timescale. For data without
	/// a presentation time, pass [`Timestamp::now`] explicitly.
	pub fn write_frame<B: crate::IntoBytes>(&mut self, timestamp: Timestamp, frame: B) -> Result<()> {
		let mut group = self.append_group()?;
		group.write_frame(timestamp, frame)?;
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
		let final_sequence = match state.max_sequence {
			Some(max) => max.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		};
		state.set_final(final_sequence)
	}

	/// Declare the track's exclusive final sequence, possibly ahead of the live edge.
	///
	/// `final_sequence` is the first sequence that will never be produced, so a track
	/// whose last group is 89 finishes at `90`. Passing a boundary beyond the current
	/// max_sequence records a known ending before the remaining groups arrive (e.g.
	/// learning a track ends at group 89 while only 87 has been received). The boundary
	/// must be strictly greater than the highest produced group, otherwise it would
	/// orphan groups that already exist ([`Error::ProtocolViolation`]).
	///
	/// Groups below `final_sequence` may still be created afterwards; groups at or above
	/// it are rejected. Consumers only see end-of-stream once the live edge reaches the
	/// boundary. Use [`Self::finish`] to finish exactly at the live edge.
	pub fn finish_at(&mut self, final_sequence: u64) -> Result<()> {
		self.modify()?.set_final(final_sequence)
	}

	/// The exclusive final sequence, once [`Self::finish`] or [`Self::finish_at`] declared one.
	///
	/// `None` while the track is still open ended. Both methods reject a second boundary, so
	/// callers that may have already declared one check here first.
	pub fn final_sequence(&self) -> Option<u64> {
		self.state.read().final_sequence
	}

	/// Abort the track with the given error.
	///
	/// Drops the cached groups so a stale [`Consumer`] can't pin them (and
	/// their frame buffers) in memory forever. Consumers that haven't drained yet
	/// surface the abort error instead of the leftover cache. Child groups are
	/// independent: a consumer that already pulled a [`group::Consumer`] keeps its
	/// own handle and can finish reading it.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut guard = self.modify()?;
		guard.abort = Some(err);
		guard.groups.clear();
		guard.datagrams.clear();
		guard.duplicates.clear();
		guard.latest_entry = None;
		guard.close();
		Ok(())
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await.map_err(|_| self.abort_reason())
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		self.state.used().await.map_err(|_| self.abort_reason())
	}

	/// Block until the track is closed or aborted, returning the cause.
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the track is closed or aborted; ready with the cause.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.state.poll_closed(waiter).map(|()| self.abort_reason())
	}

	/// The recorded abort reason, or [`Error::Dropped`] if the track closed without one.
	fn abort_reason(&self) -> Error {
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

	/// Create a [`Demand`]: a cloneable, watch-only handle to this track's
	/// subscriber demand.
	///
	/// Lets a publisher gate work (e.g. on-demand capture) on whether anyone is
	/// subscribed, without the ability to publish frames or close the track. The
	/// handle is weak, so holding one neither keeps the track alive nor pins its
	/// cached groups.
	pub fn demand(&self) -> Demand {
		Demand {
			name: self.name.clone(),
			state: self.state.weak(),
		}
	}

	/// Get a consumer handle for this in-process track.
	///
	/// Unlike a wire subscription, the info is already known, so a subscription
	/// opened from this handle resolves immediately.
	pub fn consume(&self) -> Consumer {
		Consumer::plain(self.name.clone(), self.state.consume())
	}

	/// Subscribing to this in-process track, resolving synchronously.
	///
	/// The info is fixed at creation, so there's nothing to wait for (no
	/// SUBSCRIBE_OK round trip). Pass `None` for [`Subscription::default`].
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> Subscriber {
		let preferences = subscription.into().unwrap_or_default();

		// Info is fixed at creation and survives a close/abort, so read it without
		// requiring a live producer state. If the track already ended, the returned
		// subscriber surfaces the close/abort on its first read; the preferences are
		// simply never registered (nothing aggregates them anymore).
		let info = self
			.state
			.read()
			.info
			.as_ref()
			.expect("producer always has info")
			.clone();
		let subscription = kio::Producer::new(preferences);
		register_subscription(self.state.read(), &subscription);

		Subscriber {
			name: self.name.clone(),
			info,
			inner: SubscriberKind::Plain(PlainSubscriber {
				state: self.state.consume(),
				subscription,
				index: 0,
				datagram_index: 0,
				min_sequence: 0,
				next_sequence: 0,
				end_sequence: None,
			}),
			// A producer-side (in-process) subscribe is not egress: stay untagged.
			stats: stats::Scope::default(),
			_stats_sub: stats::Subscription::default(),
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
	///
	/// The aggregate's [`Subscription::latency_max`] is clamped to this track's
	/// [`Info::latency_max`]: no subscriber can wait for a late group longer than the
	/// publisher keeps it.
	pub fn subscription(&self) -> Option<Subscription> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.latency_bound());
		drop(state);
		snapshot_subscription(&subs, bound)
	}

	/// Poll counterpart to [`subscription_changed`](Self::subscription_changed): the
	/// aggregate subscription whenever it changes, or `None` once nobody is subscribed.
	/// Errors once the track is aborted.
	pub fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Subscription>>> {
		// Surface an abort as the stream ending. `poll_closed` parks on the closed
		// waiters, so per-group churn on the track state never wakes this poll.
		if self.state.poll_closed(waiter).is_ready() {
			let abort = self.state.read().abort.clone();
			return Poll::Ready(Err(abort.unwrap_or(Error::Dropped)));
		}

		// Read the bound before locking `subs`, so the aggregation never nests the two locks.
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.latency_bound());
		drop(state);

		let prev = &self.prev_subscription;
		let mut combined = None;
		let mut guard = match subs.poll(waiter, |subs| {
			let next = combined_subscription(subs, bound, waiter);
			if &next == prev {
				Poll::Pending
			} else {
				combined = next;
				Poll::Ready(())
			}
		}) {
			Poll::Ready(guard) => guard,
			Poll::Pending => return Poll::Pending,
		};
		// The aggregate changed: prune any closed subscribers now that we hold the lock.
		guard.retain(|sub| !sub.is_closed());
		drop(guard);
		self.prev_subscription = combined.clone();
		Poll::Ready(Ok(combined))
	}

	/// Poll for the producer becoming unused (every consumer dropped).
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}

	/// Create a [`Dynamic`] handle that serves on-demand fetches of uncached
	/// (old) groups. Most producers never need this; a relay creates one to fetch
	/// past groups from upstream.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(self.name.clone(), self.state.clone())
	}

	fn modify(&self) -> Result<kio::Mut<'_, TrackState>> {
		TrackState::modify(&self.state)
	}
}

/// Pop the next queued group fetch off the fetch queue and wrap it in a
/// [`GroupRequest`] bound to a fresh producer handle. Shared by every
/// [`Dynamic`] handle on the track.
fn poll_requested_group(
	state: &kio::Producer<TrackState>,
	fetch: &kio::Shared<FetchState>,
	waiter: &kio::Waiter,
) -> Poll<Result<GroupRequest>> {
	// Prefer serving a queued fetch, even if the track has since aborted.
	if let Poll::Ready(mut guard) = fetch.poll(waiter, |fetch| {
		if fetch.has_queued() {
			Poll::Ready(())
		} else {
			Poll::Pending
		}
	}) {
		let sequence = guard.pop().expect("predicate guaranteed a request");
		// The popped attempt stays pending, so a fetch in the window between hand-off
		// and accept joins it instead of queueing a duplicate.
		// `GroupRequest::{accept, reject, drop}` removes the entry.
		let pending = guard.get(&sequence).expect("popped key must be pending");
		let priority = pending.priority;
		let result = pending.result.clone();
		drop(guard);
		return Poll::Ready(Ok(GroupRequest {
			state: state.clone(),
			fetch: fetch.clone(),
			sequence,
			priority,
			result,
			done: false,
		}));
	}

	// No fetch queued: surface a track abort so the handler loop can exit.
	match state.poll_ref(waiter, |state| match &state.abort {
		Some(err) => Poll::Ready(err.clone()),
		None => Poll::Pending,
	}) {
		Poll::Ready(Ok(err)) => Poll::Ready(Err(err)),
		Poll::Ready(Err(closed)) => Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped))),
		Poll::Pending => Poll::Pending,
	}
}

/// Serves on-demand fetches of uncached (old) groups for a track, the group-level
/// analogue of [`broadcast::Dynamic`].
///
/// Most tracks never serve old content, so this capability lives on a dedicated
/// handle rather than [`Producer`]: a relay creates one (via
/// [`Producer::dynamic`] or [`Request::dynamic`]) to pull past groups
/// from upstream. While at least one is alive the track will block a cache-miss
/// [`Consumer::fetch_group`] waiting to be served; with none, an accepted track's
/// miss fails fast with [`Error::NotFound`].
pub struct Dynamic {
	name: Arc<str>,
	// Kept to insert served groups into the cache and observe track abort.
	state: kio::Producer<TrackState>,
	// The fetch queue this handle drains; its `dynamic` count gates `fetch_group`.
	fetch: kio::Shared<FetchState>,
}

impl Dynamic {
	fn new(name: Arc<str>, state: kio::Producer<TrackState>) -> Self {
		let fetch = state.read().fetch.clone();
		fetch.lock().add_handler();
		Self { name, state, fetch }
	}

	/// The track's name, unique within its broadcast.
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

	/// Poll counterpart to [`requested_group`](Self::requested_group).
	pub fn poll_requested_group(&self, waiter: &kio::Waiter) -> Poll<Result<GroupRequest>> {
		poll_requested_group(&self.state, &self.fetch, waiter)
	}

	/// Poll for the track becoming unused (every consumer dropped).
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}
}

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Count each live handle (mirrors `broadcast::Dynamic`).
		self.fetch.lock().add_handler();
		Self {
			name: self.name.clone(),
			state: self.state.clone(),
			fetch: self.fetch.clone(),
		}
	}
}

impl Drop for Dynamic {
	fn drop(&mut self) {
		// Unlike `broadcast::Dynamic`, dropping the last handle doesn't abort the track:
		// a live `Producer` may still be serving the subscription. It just stops fetch
		// serving. Queued attempts no handler will ever pop are dropped, closing their
		// result channels so every joined `Fetching` resolves NotFound; an attempt
		// already handed to a handler stays, resolved by its `GroupRequest` instead.
		let mut fetch = self.fetch.lock();
		if fetch.remove_handler() {
			fetch.drain_queued();
		}
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		// The last producer going away without finishing is an abrupt teardown:
		// release the cached groups so a stale consumer can't pin them (and their
		// frame buffers) forever, the same as an explicit abort. A cleanly
		// finished track keeps its cache so consumers can still drain it.
		if !self.state.is_last() {
			return;
		}
		// The last ingress producer closing the track ends its subscription.
		self.stats.close_subscription();
		if let Ok(mut state) = self.state.write()
			&& state.final_sequence.is_none()
		{
			// Dropped without finish() or abort(), so consumers will see
			// Error::Dropped instead of a clean end. Deliberate ends go through
			// finish()/abort().
			tracing::warn!(
				track = %self.name(),
				"track::Producer dropped without finish() or abort()"
			);
			state.groups.clear();
			state.datagrams.clear();
			state.duplicates.clear();
			state.latest_entry = None;
		}
	}
}

/// Aggregate every live subscriber's preferences into the most demanding request.
///
/// Read-only: iterates the subscriptions immutably and registers `waiter` on each, so a
/// preference update (or a subscriber dropping) wakes the caller's poll. Callers decide
/// readiness from the returned value, then prune closed subscribers through the `Mut`.
fn combined_subscription(subs: &Subscriptions, bound: Option<Duration>, waiter: &kio::Waiter) -> Option<Subscription> {
	let mut combined = None;
	for sub in subs.iter() {
		// A closed consumer means the subscriber dropped: it holds no live demand.
		// `Consumer::poll` evaluates the closure before the closed flag, so it would
		// still replay the final value into the aggregate; skip it explicitly so a
		// departed subscriber can't keep the aggregate pinned to its last request.
		if sub.is_closed() {
			continue;
		}
		if let Poll::Ready(Ok(sub)) = sub.poll(waiter, |sub| sub.poll_combined(&combined)) {
			combined = Some(sub);
		}
	}
	clamp_combined(combined, bound)
}

/// A non-blocking aggregate of the current subscriptions, without arming any waiter.
fn snapshot_subscription(subs: &kio::Shared<Subscriptions>, bound: Option<Duration>) -> Option<Subscription> {
	let mut combined: Option<Subscription> = None;
	for sub in subs.read().iter() {
		// Skip dropped subscribers, matching `combined_subscription`.
		if sub.is_closed() {
			continue;
		}
		if let Poll::Ready(merged) = sub.read().poll_combined(&combined) {
			combined = Some(merged);
		}
	}
	clamp_combined(combined, bound)
}

/// Clamp the aggregate's latency budget to the publisher's window: nobody can wait for a
/// late group longer than the publisher keeps it around.
///
/// The single clamp point. Subscribers hold their preferences verbatim, so what they asked
/// for stays readable, and clamping the aggregate is equivalent to clamping each subscriber
/// first (`min` distributes over the `max` that combines them). `bound` is `None` on a track
/// whose info isn't known yet (an unaccepted [`Request`]), which imposes no window.
fn clamp_combined(combined: Option<Subscription>, bound: Option<Duration>) -> Option<Subscription> {
	let mut combined = combined?;
	if let Some(bound) = bound {
		combined.latency_max = combined.latency_max.min(bound);
	}
	Some(combined)
}

/// Register a subscription if the track is live: clone the shared list out of the
/// state, release the track lock, then push under the list's own lock. A closed
/// track skips the push; nothing aggregates the preferences anymore.
fn register_subscription(state: kio::Ref<'_, TrackState>, subscription: &kio::Producer<Subscription>) {
	if state.is_closed() {
		return;
	}
	let subs = state.subscriptions.clone();
	drop(state);
	subs.lock().push(subscription.consume());
}

/// A weak reference to a track that doesn't prevent auto-close.
#[derive(Clone)]
pub(crate) struct TrackWeak {
	name: Arc<str>,
	state: kio::ProducerWeak<TrackState>,
}

impl TrackWeak {
	pub fn consume(&self) -> Consumer {
		Consumer::plain(self.name.clone(), self.state.consume())
	}

	/// The shared name handle, for use as a broadcast lookup key (clone is a
	/// refcount bump, and the same `Arc` is shared with the track's handles).
	pub(crate) fn name(&self) -> &Arc<str> {
		&self.name
	}

	/// Whether anyone is consuming the track right now. A closed track doesn't
	/// count even if consumers linger to drain its cache: no new work is owed.
	pub(crate) fn is_used(&self) -> bool {
		!self.state.is_closed() && self.state.is_used()
	}

	/// Park `waiter` for the next consumer appearing; a no-op once one exists.
	/// Feeds [`crate::broadcast::Demand`], which recomputes on wake.
	pub(crate) fn poll_used(&self, waiter: &kio::Waiter) {
		let _ = self.state.poll_used(waiter);
	}

	/// Park `waiter` for the last consumer (or the track) going away; a no-op
	/// once none remain. Feeds [`crate::broadcast::Demand`].
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) {
		let _ = self.state.poll_unused(waiter);
	}
}

impl super::WeakEntry for TrackWeak {
	fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// A cloneable, watch-only handle to a track's subscriber demand.
///
/// Obtained from [`Producer::demand`]. A publisher uses it to react to
/// whether anyone is subscribed (on-demand capture / encoding) without being able
/// to publish frames or close the track. It's a weak handle, so it neither keeps
/// the track alive nor pins its cached groups; once the owning [`Producer`]
/// goes away, [`used`](Self::used) / [`unused`](Self::unused) report the track's
/// closure.
#[derive(Clone)]
pub struct Demand {
	name: Arc<str>,
	state: kio::ProducerWeak<TrackState>,
}

impl Demand {
	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		self.state.used().await.map_err(|_| self.abort_reason())
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await.map_err(|_| self.abort_reason())
	}

	/// Block until the track is closed or aborted, returning the cause.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.abort_reason()
	}

	/// The recorded abort reason, or [`Error::Dropped`] if the track closed without one.
	fn abort_reason(&self) -> Error {
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

/// A handle to a single track within a broadcast.
///
/// Obtained from [`broadcast::Consumer::track`]. Holding it sends nothing
/// to the publisher; it just names a track you can [`subscribe`](Self::subscribe)
/// to (a live, ongoing stream of groups) later. The same handle can be subscribed
/// to multiple times, and clones are cheap.
///
/// A track reached through a route-fed broadcast is *spliced*: it is backed by one
/// or more per-session tracks joined at group boundaries, and this handle reads
/// across them transparently.
#[derive(Clone)]
pub struct Consumer {
	name: Arc<str>,
	inner: ConsumerKind,
	// Egress stats scope, set by a tagged [`broadcast::Consumer`] via
	// [`Self::with_stats`]. Empty (no-op) for an untagged track.
	stats: stats::Scope,
}

#[derive(Clone)]
enum ConsumerKind {
	Plain(kio::Consumer<TrackState>),
	Spliced(super::resume::Consumer),
}

impl Consumer {
	fn plain(name: Arc<str>, state: kio::Consumer<TrackState>) -> Self {
		Self {
			name,
			inner: ConsumerKind::Plain(state),
			stats: stats::Scope::default(),
		}
	}

	/// A consumer over a spliced logical track (a route-fed broadcast's track).
	pub(crate) fn spliced(name: Arc<str>, resume: super::resume::Consumer) -> Self {
		Self {
			name,
			inner: ConsumerKind::Spliced(resume),
			stats: stats::Scope::default(),
		}
	}

	/// Attach an egress stats scope, inherited by the subscriptions, fetches, and
	/// groups derived from this handle. Called by a tagged [`broadcast::Consumer`].
	pub(crate) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Open a live subscription.
	///
	/// Registers the subscription on the track and returns a [`kio::Pending`] that resolves to the
	/// [`Subscriber`] once the track info is available, or the track's abort error (or
	/// [`Error::Dropped`]) if it is already closed.
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> kio::Pending<Subscribing> {
		let subscription = kio::Producer::new(subscription.into().unwrap_or_default());

		let inner = match &self.inner {
			ConsumerKind::Plain(state) => {
				// Register the subscription if the track is live. If it is already closed, the
				// returned future resolves to the abort error via `Subscribing::poll_ok`.
				register_subscription(state.read(), &subscription);
				SubscribingKind::Plain(state.clone())
			}
			// A spliced subscription registers per segment once the subscriber polls.
			ConsumerKind::Spliced(resume) => SubscribingKind::Spliced(resume.clone()),
		};

		kio::Pending::new(Subscribing {
			name: self.name.clone(),
			inner,
			subscription,
			stats: self.stats.clone(),
		})
	}

	// Peek at a cached group by sequence without blocking, or `None` if it isn't in the
	// cache. A test hook for asserting cache state; the library reads
	// `TrackState::cached_group` directly, and callers want `fetch_group`.
	#[cfg(test)]
	pub(crate) fn peek_group(&self, sequence: u64) -> Option<group::Consumer> {
		match &self.inner {
			ConsumerKind::Plain(state) => state.read().cached_group(sequence),
			// Spliced tracks have no cache of their own; peek the newest segment
			// via `fetch_group` instead.
			ConsumerKind::Spliced(_) => None,
		}
	}

	/// Fetching a single past group, without holding a live subscription.
	///
	/// Returns a [`kio::Pending`] that resolves to the [`group::Consumer`]:
	/// immediately if the group is cached, otherwise once a [`Dynamic`] serves
	/// the request (a wire FETCH for a relay). `options` accepts `None`, a [`group::Fetch`],
	/// or `group::Fetch::default()`.
	///
	/// The returned future resolves to [`Error::NotFound`] when the group can never be served
	/// (past the final sequence, or no [`Dynamic`] on the track), or the track's abort error
	/// if it's already closed. Concurrent fetches for the same sequence coalesce onto one
	/// handler request.
	pub fn fetch_group(&self, sequence: u64, options: impl Into<Option<group::Fetch>>) -> kio::Pending<Fetching> {
		let options = options.into().unwrap_or_default();

		// One fetch per calling context, counted here (coalesced upstream work is
		// still one request served). Independent of `subscriptions` and the viewer
		// refcount.
		self.stats.fetch();

		let state = match &self.inner {
			ConsumerKind::Plain(state) => state,
			// Spliced: routed to the newest segment's (plain) track, waiting for a
			// segment to exist if no route has served the track yet.
			ConsumerKind::Spliced(resume) => {
				return kio::Pending::new(Fetching {
					inner: FetchingKind::Spliced(resume.fetch_group(sequence, options)),
					stats: self.stats.clone(),
				});
			}
		};

		let mut result = None;

		// Queue a request only when the group isn't already resolvable from the track
		// (cached, aborted, or past-final all resolve through `Fetching::poll` without
		// a queue entry).
		let (fetch, unresolved) = {
			let state = state.read();
			(state.fetch.clone(), state.poll_fetch_cached(sequence).is_pending())
		};

		if unresolved {
			let mut fetch = fetch.lock();
			if let Some(pending) = fetch.join(&sequence) {
				// Join the in-flight attempt for this sequence (queued or already being
				// served): share its result channel, raising its priority if ours is higher.
				pending.priority = pending.priority.max(options.priority);
				result = Some(pending.result.consume());
			} else {
				// Queue a new attempt. The handler gate is atomic with a handler
				// dropping (no fetch stranded on a queue nobody drains); with no
				// handler, `Fetching::poll` fails fast instead.
				let producer = kio::Producer::<FetchOutcome>::default();
				let consumer = producer.consume();
				let attempt = PendingFetch {
					priority: options.priority,
					result: producer,
				};
				if fetch.insert(sequence, attempt).is_ok() {
					result = Some(consumer);
				}
			}
		}

		kio::Pending::new(Fetching {
			inner: FetchingKind::Plain {
				state: state.clone(),
				fetch,
				sequence,
				result,
			},
			stats: self.stats.clone(),
		})
	}

	/// Resolve the track's [`Info`] without subscribing.
	///
	/// A [`Consumer`] is a lazy handle, so the info may not be known yet: this waits
	/// for the producer to [`Request::accept`] the track (a wire TRACK_INFO round-trip
	/// for a relay), and errors with the track's abort error if it closes first.
	/// [`Subscriber::info`] is the already-resolved counterpart.
	pub fn info(&self) -> kio::Pending<Querying> {
		kio::Pending::new(Querying {
			inner: match &self.inner {
				ConsumerKind::Plain(state) => QueryingKind::Plain(state.clone()),
				ConsumerKind::Spliced(resume) => QueryingKind::Spliced(resume.clone()),
			},
		})
	}

	/// Return the latest group sequence in the track, or `None` before any group.
	pub fn latest(&self) -> Option<u64> {
		match &self.inner {
			ConsumerKind::Plain(state) => state.read().max_sequence,
			ConsumerKind::Spliced(resume) => resume.latest(),
		}
	}

	/// Poll for the track reaching a terminal state: `Ok(())` once it is complete
	/// (the final group was produced), `Err` once it closed or aborted before
	/// completing. The origin's dispatcher uses this to tell a track that truly
	/// ended from one whose serving route died mid-stream.
	pub(crate) fn poll_complete(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		let ConsumerKind::Plain(state) = &self.inner else {
			// Spliced tracks are compositions; the dispatcher never monitors one.
			return Poll::Pending;
		};
		match ready!(state.poll(waiter, |state| {
			if state.is_complete() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		})) {
			Ok(_) => Poll::Ready(Ok(())),
			// Closed before completing. Read through the returned guard: it holds
			// the lock, so re-locking the channel here would deadlock.
			Err(closed) => Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped))),
		}
	}
}

/// The pollable state of a [`Consumer::subscribe`]; awaited via the
/// [`kio::Pending`] wrapper, whose `DerefMut` exposes [`Self::update`].
pub struct Subscribing {
	name: Arc<str>,
	inner: SubscribingKind,
	subscription: kio::Producer<Subscription>,
	stats: stats::Scope,
}

enum SubscribingKind {
	Plain(kio::Consumer<TrackState>),
	Spliced(super::resume::Consumer),
}

impl Subscribing {
	/// Poll until the peer confirms the subscription, yielding the [`Subscriber`].
	/// Errors if the track is aborted or not found.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Subscriber>> {
		match &self.inner {
			SubscribingKind::Plain(state) => {
				// Wait until the track info is available
				let info = ready!(state.poll(waiter, |state| state.poll_info()))
					.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;

				Poll::Ready(Ok(Subscriber {
					name: self.name.clone(),
					info,
					inner: SubscriberKind::Plain(PlainSubscriber {
						state: state.clone(),
						subscription: self.subscription.clone(),
						index: 0,
						datagram_index: 0,
						min_sequence: 0,
						next_sequence: 0,
						end_sequence: None,
					}),
					stats: self.stats.clone(),
					_stats_sub: self.stats.subscribe(),
				}))
			}
			SubscribingKind::Spliced(resume) => {
				// Resolved from the first segment's track. The publisher's latency
				// window is applied to each per-session aggregate, not here.
				let info = ready!(resume.poll_info(waiter))?;

				Poll::Ready(Ok(Subscriber {
					name: self.name.clone(),
					info,
					inner: SubscriberKind::Spliced(Box::new(resume.subscribe_shared(self.subscription.clone()))),
					stats: self.stats.clone(),
					_stats_sub: self.stats.subscribe(),
				}))
			}
		}
	}

	/// Change the subscription preferences before (or after) it resolves.
	///
	/// Returns [`Error::Closed`] if the track already ended; the update is
	/// meaningless at that point and can usually be ignored.
	pub fn update(&mut self, subscription: Subscription) -> Result<()> {
		let mut state = self.subscription.write().map_err(|_| Error::Closed)?;
		*state = subscription;
		Ok(())
	}
}

impl kio::Pollable for Subscribing {
	type Output = Result<Subscriber>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// The pollable state of a [`Consumer::info`]; awaited via the
/// [`kio::Pending`] wrapper.
pub struct Querying {
	inner: QueryingKind,
}

enum QueryingKind {
	Plain(kio::Consumer<TrackState>),
	Spliced(super::resume::Consumer),
}

impl Querying {
	/// Poll until the track's [`Info`] is known, without subscribing to its groups.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Info>> {
		match &self.inner {
			QueryingKind::Plain(state) => {
				// Wait until the track info is available
				let info = ready!(state.poll(waiter, |state| state.poll_info()))
					.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;
				Poll::Ready(Ok(info))
			}
			QueryingKind::Spliced(resume) => resume.poll_info(waiter),
		}
	}
}

impl kio::Pollable for Querying {
	type Output = Result<Info>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// A consumer's request for a single past group, handed to a handler via
/// [`Dynamic::requested_group`].
///
/// The handler fulfills it by calling [`Self::accept`], which inserts the group
/// into the track cache (resolving every [`Consumer::fetch_group`] that joined the
/// attempt) and returns a [`group::Producer`] to fill. A relay typically opens a wire
/// FETCH, reads FETCH_OK, then accepts. The request carries its own producer handle,
/// so it works the same whether or not the track has been accepted yet.
pub struct GroupRequest {
	state: kio::Producer<TrackState>,
	// To remove this attempt from the fetch state once it resolves.
	fetch: kio::Shared<FetchState>,
	sequence: u64,
	priority: u8,
	// Rejections route back to every joined `Fetching`.
	result: kio::Producer<FetchOutcome>,
	done: bool,
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
	/// [`Consumer::fetch_group`], and return a [`group::Producer`] to fill.
	///
	/// The group's timescale comes from the track's [`Info`]. `info` sets that
	/// info if the track hasn't been accepted yet (a fetch with no live subscription),
	/// and is ignored once accepted. Returns [`Error::Duplicate`] if the group is
	/// already present, or the track's abort error if it closed while pending.
	pub fn accept(mut self, info: impl Into<Option<Info>>) -> Result<group::Producer> {
		self.done = true;
		// Cache the group before removing the attempt: the joined fetches resolve
		// through the cache, and removal closes their result channel (which alone
		// would read as NotFound).
		let res = TrackState::modify(&self.state)
			.and_then(|mut state| state.insert_group_request(self.sequence, info.into()));
		self.remove();
		res
	}

	/// Reject the fetch, resolving every joined [`Consumer::fetch_group`] with `err`.
	pub fn reject(mut self, err: Error) {
		self.done = true;
		// Remove before writing, so a fetch arriving now starts a fresh attempt
		// instead of joining a rejected one.
		self.remove();
		if let Ok(mut outcome) = self.result.write() {
			outcome.rejected = Some(err);
		}
	}

	/// Remove this attempt from the fetch state, unless a newer attempt for the same
	/// sequence has already replaced it.
	fn remove(&self) {
		self.fetch
			.lock()
			.remove_if(&self.sequence, |pending| pending.result.same_channel(&self.result));
	}
}

impl Drop for GroupRequest {
	fn drop(&mut self) {
		if self.done {
			return;
		}
		self.remove();
		if let Ok(mut outcome) = self.result.write() {
			outcome.rejected = Some(Error::Dropped);
		}
	}
}

/// The pollable state of a [`Consumer::fetch_group`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the
/// [`group::Consumer`] once the group lands in the track's cache (already present,
/// or produced after a wire FETCH), or [`Error::NotFound`] if it can never exist.
pub struct Fetching {
	inner: FetchingKind,
	// Egress stats scope, so the resolved group carries a payload meter (and counts
	// as one delivered group). Empty (no-op) for an untagged track.
	stats: stats::Scope,
}

enum FetchingKind {
	Plain {
		state: kio::Consumer<TrackState>,
		fetch: kio::Shared<FetchState>,
		sequence: u64,
		// The joined attempt's result channel; `None` when no handler existed to queue on.
		result: Option<kio::Consumer<FetchOutcome>>,
	},
	/// A spliced track's fetch: waits for a segment, then fetches from it.
	Spliced(kio::Pending<super::resume::Fetching>),
}

impl kio::Pollable for Fetching {
	type Output = Result<group::Consumer>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		let (state, fetch, sequence, result) = match &self.inner {
			FetchingKind::Plain {
				state,
				fetch,
				sequence,
				result,
			} => (state, fetch, *sequence, result.as_ref()),
			FetchingKind::Spliced(spliced) => {
				// A fetched group is metered here (once), at the tagged handle: the
				// spliced source track it comes from is the origin's own, untagged.
				return kio::Pollable::poll(&**spliced, waiter)
					.map(|res| res.map(|group| group.with_meter(self.stats.meter())));
			}
		};

		// Track side: the cached group, the abort error, or past-final. The outer
		// error is the channel closing without any of those.
		match state.poll(waiter, |state| state.poll_fetch_cached(sequence)) {
			Poll::Ready(Ok(res)) => return Poll::Ready(res.map(|group| group.with_meter(self.stats.meter()))),
			Poll::Ready(Err(closed)) => {
				return Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped)));
			}
			Poll::Pending => {}
		}

		// Handler side.
		let Some(result) = result else {
			// Never queued: no handler existed when the fetch was made. Fail fast while
			// that's still true; a handler that appeared since may yet fill the cache.
			return match fetch.poll(waiter, |fetch| match fetch.has_handlers() {
				false => Poll::Ready(()),
				true => Poll::Pending,
			}) {
				Poll::Ready(_guard) => Poll::Ready(Err(Error::NotFound)),
				Poll::Pending => Poll::Pending,
			};
		};

		// A written rejection fails every joined fetch. The channel closing without
		// one means the attempt was dropped unserved (its handlers went away).
		match result.poll(waiter, |outcome| match &outcome.rejected {
			Some(err) => Poll::Ready(err.clone()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(err)) => Poll::Ready(Err(err)),
			Poll::Ready(Err(_closed)) => Poll::Ready(Err(Error::NotFound)),
			Poll::Pending => Poll::Pending,
		}
	}
}

/// A live subscription to a track, used to read its groups.
///
/// Created via [`Consumer::subscribe`](Consumer::subscribe), or
/// directly from a [`Producer`] for an in-process track. Carries this
/// subscriber's [`Subscription`] preferences, which feed the producer's aggregate.
///
/// # Local cursor vs wire preference
///
/// Group bounds exist at two levels, and setting one does not imply the other:
///
/// - [`Self::start_at`] / [`Self::end_at`] move **this subscriber's read cursor**. They
///   filter exactly what this handle returns and are invisible to the publisher.
/// - [`Subscription::group_start`] / [`Subscription::group_end`], set via [`Self::update`],
///   are a **request to the publisher**. They're aggregated across every live subscriber
///   (earliest start, widest end), so they say what the publisher should send, not what
///   this subscriber sees.
///
/// They stay separate because their scopes differ: a subscriber can't filter by the
/// aggregate, since another subscriber can widen it, and the publisher can't honor a
/// cursor it's never told about. So setting only the cursor still transfers the skipped
/// groups, and setting only the preference still returns groups another subscriber asked
/// for. Set both to skip them *and* avoid the transfer.
pub struct Subscriber {
	name: Arc<str>,
	info: Info,
	inner: SubscriberKind,
	// Egress stats scope, used to meter the groups this subscriber reads. Empty
	// (no-op) for an untagged track.
	stats: stats::Scope,
	// The subscription guard: bumps `subscriptions` (and the egress viewer refcount)
	// while held, closing them on drop. Empty (no-op) for an untagged track.
	_stats_sub: stats::Subscription,
}

enum SubscriberKind {
	Plain(PlainSubscriber),
	// Boxed: the spliced cursor set dwarfs the plain cursor.
	Spliced(Box<super::resume::Subscriber>),
}

/// The cursor state for a subscription over a single (per-session) track.
struct PlainSubscriber {
	state: kio::Consumer<TrackState>,

	subscription: kio::Producer<Subscription>,
	/// Arrival-order cursor used by `recv_group`.
	index: usize,
	/// Arrival-order cursor used by `recv_datagram`, independent of groups.
	datagram_index: usize,
	/// Minimum sequence to return from any `recv` method. Set by `start_at`.
	min_sequence: u64,
	/// One past the highest sequence returned by `next_group`.
	/// Used only by that method to skip late arrivals; does not affect `recv_group`.
	next_sequence: u64,
	/// Inclusive upper sequence bound for `next_group`. `None` means no cap. Set by
	/// `end_at`; can be raised, lowered, or unset at any time. Groups beyond the
	/// cap stay in the producer's cache and become eligible again when the cap
	/// rises (or is removed).
	end_sequence: Option<u64>,
}

impl PlainSubscriber {
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

	fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let Some((consumer, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_group(self.index, self.min_sequence))?)
		else {
			return Poll::Ready(Ok(None));
		};

		self.index = found_index + 1;
		Poll::Ready(Ok(Some(consumer)))
	}

	fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		let Some((datagram, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_datagram(self.datagram_index))?)
		else {
			return Poll::Ready(Ok(None));
		};

		self.datagram_index = found_index + 1;
		self.next_sequence = self.next_sequence.max(datagram.sequence.saturating_add(1));
		Poll::Ready(Ok(Some(datagram)))
	}

	fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let floor = self.next_sequence.max(self.min_sequence);
		let Some(group) = ready!(self.poll(waiter, |state| state.poll_next_in_range(floor, self.end_sequence))?) else {
			return Poll::Ready(Ok(None));
		};
		self.next_sequence = group.sequence.saturating_add(1);
		Poll::Ready(Ok(Some(group)))
	}

	fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
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
}

/// A cloneable handle to a subscriber's delivery preferences.
///
/// This updates the same subscription as the owning [`Subscriber`] without
/// borrowing its read cursor, so callers can change delivery priority, group
/// ordering priority, or group bounds while another task is waiting for groups.
#[derive(Clone)]
pub struct SubscriberControl {
	subscription: kio::Producer<Subscription>,
}

impl SubscriberControl {
	/// This subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.subscription.read().clone()
	}

	/// Replace this subscriber's preferences, updating the producer's aggregate.
	///
	/// Returns [`Error::Closed`] if the track already ended; the update is
	/// meaningless at that point and can usually be ignored.
	pub fn update(&self, subscription: Subscription) -> Result<()> {
		let mut state = self.subscription.write().map_err(|_| Error::Closed)?;
		*state = subscription;
		Ok(())
	}
}

impl Subscriber {
	/// The track's [`Info`], resolved when the subscription was established.
	///
	/// Free, unlike [`Consumer::info`]: subscribing already waited for the info
	/// (SUBSCRIBE_OK on the wire), so a subscriber always has it.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Create a handle for updating this subscriber's delivery preferences.
	pub fn control(&self) -> SubscriberControl {
		SubscriberControl {
			subscription: match &self.inner {
				SubscriberKind::Plain(plain) => plain.subscription.clone(),
				SubscriberKind::Spliced(spliced) => spliced.prefs(),
			},
		}
	}

	/// Poll for the next group in arrival order, without blocking.
	///
	/// Returns every group exactly once in the order it landed on the wire, which may be
	/// out of sequence due to network reordering or loss. Use [`Self::poll_next_group`] if
	/// you only want groups whose sequence number is higher than any previously returned.
	///
	/// Returns `Poll::Ready(Ok(Some(group)))` when a group is available,
	/// `Poll::Ready(Ok(None))` when the track is finished,
	/// `Poll::Ready(Err(e))` when the track has been aborted, or
	/// `Poll::Pending` when no group is available yet.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let meter = self.stats.meter();
		let res = match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.poll_recv_group(waiter),
			SubscriberKind::Spliced(spliced) => spliced.poll_recv_group(waiter),
		};
		res.map(|res| res.map(|group| group.map(|group| group.with_meter(meter))))
	}

	/// Receive the next group in arrival order.
	///
	/// Every group is returned exactly once, in the order it landed on the wire, which may
	/// be out of sequence due to network reordering or loss. Use [`Self::next_group`] if you
	/// only want groups whose sequence number is higher than any previously returned.
	pub async fn recv_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Poll for the next datagram in arrival order, without blocking.
	///
	/// Datagrams are a separate best-effort channel from groups (see
	/// [`Producer::append_datagram`]); they share only the sequence namespace. A consumer
	/// that falls too far behind silently loses the oldest datagrams.
	/// Returning a datagram advances [`Self::poll_next_group`] past that sequence.
	///
	/// Returns `Poll::Ready(Ok(Some(datagram)))` when one is available,
	/// `Poll::Ready(Ok(None))` when the track is finished, `Poll::Ready(Err(e))` when the track
	/// is aborted, or `Poll::Pending` when none is buffered yet.
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		let meter = self.stats.meter();
		let res = match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.poll_recv_datagram(waiter),
			SubscriberKind::Spliced(spliced) => spliced.poll_recv_datagram(waiter),
		};
		// Unlike a group (metered lazily as its frames are read), a datagram is
		// delivered whole here, so count it as the single-frame group it stands in for.
		if let Poll::Ready(Ok(Some(datagram))) = &res {
			meter.datagram(datagram.payload.len() as u64);
		}
		res
	}

	/// Receive the next datagram in arrival order.
	///
	/// A best-effort channel parallel to [`Self::recv_group`]; the two share only the sequence
	/// namespace. To receive both concurrently from one subscriber, poll [`Self::poll_next_group`]
	/// (or [`Self::poll_recv_group`]) and [`Self::poll_recv_datagram`] together in a single `poll`
	/// closure (sequential `&mut` borrows), rather than awaiting the two `recv` futures at once.
	pub async fn recv_datagram(&mut self) -> Result<Option<Datagram>> {
		kio::wait(|waiter| self.poll_recv_datagram(waiter)).await
	}

	/// Poll for the next group with a higher sequence number than any previously returned.
	///
	/// Late arrivals (sequence at or below the last returned) are silently skipped, so this
	/// produces a monotonically increasing sequence at the cost of dropping out-of-order
	/// groups. Use [`Self::poll_recv_group`] to see every group in arrival order instead.
	///
	/// Honors the cap set by [`Self::end_at`]: groups with sequence past the cap are left
	/// in the producer's cache and become eligible again if the cap is raised or removed.
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let meter = self.stats.meter();
		let res = match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.poll_next_group(waiter),
			SubscriberKind::Spliced(spliced) => spliced.poll_next_group(waiter),
		};
		res.map(|res| res.map(|group| group.map(|group| group.with_meter(meter))))
	}

	/// Return the next group with a higher sequence number than any previously returned.
	///
	/// Late arrivals (sequence at or below the last returned) are silently skipped, so this
	/// produces a monotonically increasing sequence at the cost of dropping out-of-order
	/// groups. Use [`Self::recv_group`] to see every group in arrival order instead.
	pub async fn next_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_next_group(waiter)).await
	}

	/// A helper that calls [`Self::poll_next_group`] and returns its first frame
	/// (timestamp and payload), skipping the rest of the group. Intended for
	/// single-frame groups (see [`Producer::write_frame`]).
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		let meter = self.stats.meter();
		let res = match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.poll_read_frame(waiter),
			SubscriberKind::Spliced(spliced) => spliced.poll_read_frame(waiter),
		};
		// This helper collapses a group to its first frame: count the group, the one
		// frame, and the bytes actually read.
		if let Poll::Ready(Ok(Some(frame))) = &res {
			meter.group();
			meter.frames(1);
			meter.bytes(frame.payload.len() as u64);
		}
		res
	}

	/// Read a single full frame (timestamp and payload) from the next group in
	/// sequence order.
	///
	/// See [`Self::poll_read_frame`] for semantics.
	pub async fn read_frame(&mut self) -> Result<Option<frame::Frame>> {
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Whether `other` was cloned from this subscriber (shares the same underlying state).
	pub fn is_clone(&self, other: &Self) -> bool {
		match (&self.inner, &other.inner) {
			(SubscriberKind::Plain(a), SubscriberKind::Plain(b)) => a.state.same_channel(&b.state),
			(SubscriberKind::Spliced(a), SubscriberKind::Spliced(b)) => a.is_clone(b),
			_ => false,
		}
	}

	/// Poll for the track's declared final sequence, without blocking.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.poll(waiter, |state| state.poll_finished()),
			SubscriberKind::Spliced(spliced) => spliced.poll_finished(waiter),
		}
	}

	/// Block until the track declares its end, returning the exclusive final sequence
	/// (also the total group count), or the cause on an abort.
	///
	/// Resolves as soon as the boundary is known, which may be ahead of the live edge
	/// when the producer finished via [`Producer::finish_at`]. This reports the declared
	/// end, not that every group has arrived: drive [`Self::recv_group`] /
	/// [`Self::next_group`] until they yield `None` to observe the track fully drained.
	pub async fn finished(&mut self) -> Result<u64> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Start this subscriber's read cursor at the given sequence.
	///
	/// A local filter, not a request: it doesn't tell the publisher anything, so the
	/// skipped groups are still delivered and simply not returned. To ask the publisher
	/// to start there instead, set [`Subscription::group_start`] via [`Self::update`].
	/// See [Local cursor vs wire preference](Self#local-cursor-vs-wire-preference).
	pub fn start_at(&mut self, sequence: u64) {
		match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.min_sequence = sequence,
			SubscriberKind::Spliced(spliced) => spliced.start_at(sequence),
		}
	}

	/// Cap this subscriber's read cursor at the given sequence (inclusive), or remove the
	/// cap entirely.
	///
	/// Accepts a bare `u64` (cap), `Some(u64)`, or `None` (uncap).
	///
	/// A local filter, not a request; [`Subscription::group_end`] is the wire-level
	/// counterpart. See [Local cursor vs wire preference](Self#local-cursor-vs-wire-preference).
	///
	/// Affects [`Self::next_group`] only: groups beyond the cap stay in the producer's
	/// cache rather than being skipped past, so a later call to [`Self::end_at`] with a
	/// higher value (or `None`) makes them available again. Lowering the cap below the
	/// consumer's current cursor parks the consumer until the cap is raised.
	pub fn end_at(&mut self, sequence: impl Into<Option<u64>>) {
		match &mut self.inner {
			SubscriberKind::Plain(plain) => plain.end_sequence = sequence.into(),
			SubscriberKind::Spliced(spliced) => spliced.end_at(sequence),
		}
	}

	/// This subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.control().subscription()
	}

	/// Replace this subscriber's delivery preferences.
	///
	/// Stored verbatim; the publisher's latency window is applied to the aggregate, not
	/// here (see [`Producer::subscription`]). Returns [`Error::Closed`] if the track
	/// already ended; the update is meaningless at that point and can usually be ignored.
	pub fn update(&mut self, subscription: Subscription) -> Result<()> {
		match &mut self.inner {
			SubscriberKind::Plain(plain) => {
				let mut state = plain.subscription.write().map_err(|_| Error::Closed)?;
				*state = subscription;
			}
			SubscriberKind::Spliced(spliced) => spliced.update(subscription),
		}
		Ok(())
	}

	/// Return the latest sequence number in the track.
	pub fn latest(&self) -> Option<u64> {
		match &self.inner {
			SubscriberKind::Plain(plain) => plain.state.read().max_sequence,
			SubscriberKind::Spliced(spliced) => spliced.latest(),
		}
	}
}

/// A subscriber asked for a track this broadcast doesn't have yet.
///
/// Yielded by [`broadcast::Dynamic::requested_track`](crate::broadcast::Dynamic::requested_track),
/// or created up front with [`broadcast::Producer::reserve_track`](crate::broadcast::Producer::reserve_track).
/// Subscribers block until the request is
/// resolved: call [`accept`](Self::accept) to serve it with a [`Producer`], or
/// [`reject`](Self::reject) to fail them. Dropping it without either rejects with
/// [`Error::Dropped`].
///
/// Concurrent requests for one name are coalesced, so exactly one of these exists per
/// name at a time.
pub struct Request {
	name: Arc<str>,
	// The parent broadcast's info, threaded into the [`Producer`] on accept.
	broadcast: Arc<broadcast::Info>,
	state: kio::Producer<TrackState>,

	// The previous subscription that was combined, used to detect changes.
	prev_subscription: Option<Subscription>,

	// A requested track is served on demand, so it counts as fetch-capable from
	// birth: a consumer's cache-miss `fetch_group` waits to be served instead of
	// racing the producer (e.g. a relay) into creating its own handler. Released
	// when the request is accepted or dropped; by then the relay holds its own.
	_dynamic: Dynamic,

	// Ingress stats scope, threaded into the accepted [`Producer`]. Empty (no-op)
	// unless this request was reserved on a tagged broadcast.
	stats: stats::Scope,
}

impl Request {
	pub(crate) fn new(broadcast: Arc<broadcast::Info>, name: impl Into<Arc<str>>) -> Self {
		let name = name.into();
		let state = kio::Producer::new(TrackState {
			broadcast: broadcast.clone(),
			..Default::default()
		});
		let dynamic = Dynamic::new(name.clone(), state.clone());
		Self {
			name,
			broadcast,
			state,
			prev_subscription: None,
			_dynamic: dynamic,
			stats: stats::Scope::default(),
		}
	}

	/// Attach an ingress stats scope, applied to the [`Producer`] on accept. Set by
	/// a tagged [`broadcast::Producer::reserve_track`].
	pub(crate) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// The requested track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// A [`Consumer`] for the eventual track, usable before the request is accepted.
	pub fn consume(&self) -> Consumer {
		Consumer::plain(self.name.clone(), self.state.consume())
	}

	/// Create a [`Dynamic`] handle that serves on-demand fetches of uncached
	/// groups, before [`Self::accept`] is even called. A relay creates one to fetch
	/// past groups from upstream while (or instead of) serving a live subscription.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(self.name.clone(), self.state.clone())
	}

	/// Poll for the request becoming unused (every consumer dropped), so a relay can
	/// stop serving and drop the request.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}

	/// Serve the request with the given track, resolving every waiting subscriber.
	///
	/// The name is taken from [`Self::name`]; `info` supplies the remaining knobs
	/// (`None` for the defaults). If the track was already aborted, the returned
	/// [`Producer`] is inert: writes fail with the abort error, as if it had been
	/// aborted immediately after accepting.
	pub fn accept(self, info: impl Into<Option<Info>>) -> Producer {
		let mut info = info.into().unwrap_or_default();
		info.broadcast = self.broadcast.clone();
		// A closed state means the track was aborted under us. Mirror `reject` and
		// tolerate it: the Producer we hand back simply can't write.
		if let Ok(mut state) = self.state.write() {
			state.info = Some(info);
		}
		// Accepting the request creates the track producer: count it as one ingress
		// subscription (closed on the last producer drop). No-op when untagged.
		self.stats.open_subscription();
		Producer {
			name: self.name,
			broadcast: self.broadcast,
			state: self.state,
			prev_subscription: None,
			stats: self.stats,
		}
	}

	/// Reject the request, waking all waiting subscribers with `err`.
	pub fn reject(self, err: Error) {
		if let Ok(mut state) = self.state.write() {
			state.abort = Some(err);
		}
	}

	/// The delivery preferences aggregated across everyone waiting on this request,
	/// or `None` if nobody is waiting. Useful for sizing the track before accepting.
	pub fn subscription(&self) -> Option<Subscription> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.latency_bound());
		drop(state);
		snapshot_subscription(&subs, bound)
	}

	/// Block until the aggregate [`subscription`](Self::subscription) changes,
	/// yielding `None` once nobody is waiting.
	pub async fn subscription_changed(&mut self) -> Option<Subscription> {
		kio::wait(|waiter| self.poll_subscription_changed(waiter)).await
	}

	/// Poll counterpart to [`subscription_changed`](Self::subscription_changed).
	pub fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Option<Subscription>> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.latency_bound());
		drop(state);

		let prev = &self.prev_subscription;
		let mut combined = None;
		let mut guard = ready!(subs.poll(waiter, |subs| {
			let next = combined_subscription(subs, bound, waiter);
			if &next == prev {
				Poll::Pending
			} else {
				combined = next;
				Poll::Ready(())
			}
		}));
		// The aggregate changed: prune any closed subscribers now that we hold the lock.
		guard.retain(|sub| !sub.is_closed());
		drop(guard);
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
#[allow(missing_docs)] // test-only assertion helpers
impl Subscriber {
	pub fn assert_group(&mut self) -> group::Consumer {
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

	pub fn assert_not_closed(&mut self) {
		assert!(self.finished().now_or_never().is_none(), "should not be closed");
	}

	pub fn assert_closed(&mut self) {
		assert!(self.finished().now_or_never().is_some(), "should be closed");
	}

	// TODO assert specific errors after implementing PartialEq
	pub fn assert_error(&mut self) {
		assert!(
			self.finished().now_or_never().expect("should not block").is_err(),
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

	/// Mint a track for tests with a default parent broadcast, since tracks are
	/// normally born from a [`broadcast::Producer`].
	fn track_producer(name: impl Into<Arc<str>>, info: impl Into<Option<Info>>) -> Producer {
		Producer::new(Arc::new(broadcast::Info::default()), name, info)
	}

	/// Helper: count non-tombstoned groups in state.
	fn live_groups(state: &TrackState) -> usize {
		state.groups.iter().flatten().count()
	}

	/// Helper: get the sequence number of the first live group.
	fn first_live_sequence(state: &TrackState) -> u64 {
		state.groups.iter().flatten().next().unwrap().0.sequence
	}

	/// Helper: non-blocking datagram receive that must be ready with a datagram.
	fn recv_datagram(dg: &mut Subscriber) -> Datagram {
		dg.recv_datagram()
			.now_or_never()
			.expect("datagram would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	#[tokio::test]
	async fn append_datagram_shares_group_sequence() {
		let mut producer = track_producer("test", None);
		let ts = Timestamp::from_millis(10).unwrap();

		// Interleave groups and datagrams: they draw from one monotonic counter.
		assert_eq!(producer.append_group().unwrap().sequence, 0);
		assert_eq!(producer.append_datagram(ts, &b"a"[..]).unwrap(), 1);
		assert_eq!(producer.append_group().unwrap().sequence, 2);
		assert_eq!(producer.append_datagram(ts, &b"b"[..]).unwrap(), 3);
		assert_eq!(producer.latest(), Some(3));
	}

	#[tokio::test]
	async fn append_datagram_roundtrip() {
		let mut producer = track_producer("test", None);
		let mut dg = producer.subscribe(None);

		let ts = Timestamp::from_millis(42).unwrap();
		let seq = producer.append_datagram(ts, &b"hello"[..]).unwrap();

		let got = recv_datagram(&mut dg);
		assert_eq!(got.sequence, seq);
		assert_eq!(got.timestamp, ts);
		assert_eq!(&got.payload[..], b"hello");
	}

	#[tokio::test]
	async fn write_datagram_preserves_sequence() {
		let mut producer = track_producer("test", None);
		let mut dg = producer.subscribe(None);

		let ts = Timestamp::from_millis(5).unwrap();
		// A relay forwarding an upstream datagram keeps its sequence number.
		producer
			.write_datagram(Datagram {
				sequence: 100,
				timestamp: ts,
				payload: bytes::Bytes::from_static(b"x"),
			})
			.unwrap();

		assert_eq!(recv_datagram(&mut dg).sequence, 100);
		// max_sequence advanced, so the next appended group/datagram continues past it.
		assert_eq!(producer.append_group().unwrap().sequence, 101);
	}

	#[tokio::test]
	async fn recv_datagram_advances_ordered_group_cursor() {
		let mut producer = track_producer("test", None);
		let mut subscriber = producer.subscribe(None);
		let ts = Timestamp::from_millis(5).unwrap();

		producer
			.write_datagram(Datagram {
				sequence: 5,
				timestamp: ts,
				payload: bytes::Bytes::from_static(b"x"),
			})
			.unwrap();
		assert_eq!(recv_datagram(&mut subscriber).sequence, 5);

		producer.create_group(group::Info { sequence: 3 }).unwrap();
		producer.create_group(group::Info { sequence: 6 }).unwrap();

		let group = subscriber
			.next_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed");
		assert_eq!(group.sequence, 6);
	}

	#[tokio::test]
	async fn datagram_normalized_to_track_timescale() {
		let info = Info::default().with_timescale(Timescale::MICRO);
		let mut producer = track_producer("test", info);
		let mut dg = producer.subscribe(None);

		// Supplied at millis; stored/emitted at the track's micro timescale.
		producer
			.append_datagram(Timestamp::from_millis(2).unwrap(), &b"z"[..])
			.unwrap();
		let got = recv_datagram(&mut dg);
		assert_eq!(got.timestamp.scale(), Timescale::MICRO);
		assert_eq!(got.timestamp.value(), 2_000);
	}

	#[tokio::test]
	async fn datagram_rejects_oversized() {
		let mut producer = track_producer("test", None);
		let big = bytes::Bytes::from(vec![0u8; crate::model::datagram::MAX_DATAGRAM_PAYLOAD + 1]);
		let ts = Timestamp::from_millis(0).unwrap();
		assert!(matches!(
			producer.append_datagram(ts, big.clone()),
			Err(Error::FrameTooLarge)
		));
		assert!(matches!(
			producer.write_datagram(Datagram {
				sequence: 0,
				timestamp: ts,
				payload: big,
			}),
			Err(Error::FrameTooLarge)
		));
	}

	#[tokio::test]
	async fn datagram_fanout_to_subscribers() {
		let mut producer = track_producer("test", None);
		// Two independent subscribers, each with its own datagram cursor.
		let mut a = producer.subscribe(None);
		let mut b = producer.subscribe(None);
		let ts = Timestamp::from_millis(1).unwrap();

		producer.append_datagram(ts, &b"first"[..]).unwrap();
		producer.append_datagram(ts, &b"second"[..]).unwrap();

		// Both receive every datagram in order, independently.
		assert_eq!(&recv_datagram(&mut a).payload[..], b"first");
		assert_eq!(&recv_datagram(&mut a).payload[..], b"second");
		assert_eq!(&recv_datagram(&mut b).payload[..], b"first");
		assert_eq!(&recv_datagram(&mut b).payload[..], b"second");
	}

	#[tokio::test]
	async fn datagram_evicts_stale() {
		tokio::time::pause();

		let mut producer = track_producer("test", None);
		let mut dg = producer.subscribe(None);
		let ts = Timestamp::from_millis(0).unwrap();

		producer.append_datagram(ts, &b"old"[..]).unwrap(); // sequence 0

		// Age past the send-buffer window, then push a fresh datagram: the stale one is evicted.
		tokio::time::advance(MAX_DATAGRAM_AGE + Duration::from_millis(10)).await;
		producer.append_datagram(ts, &b"new"[..]).unwrap(); // sequence 1

		// A lagging consumer resumes at the oldest still-buffered datagram (the fresh one).
		let got = recv_datagram(&mut dg);
		assert_eq!(got.sequence, 1);
		assert_eq!(&got.payload[..], b"new");
	}

	#[tokio::test]
	async fn datagram_recv_pends_until_written() {
		let mut producer = track_producer("test", None);
		let mut dg = producer.subscribe(None);

		assert!(
			dg.recv_datagram().now_or_never().is_none(),
			"should block with no datagrams"
		);

		producer
			.append_datagram(Timestamp::from_millis(0).unwrap(), &b"go"[..])
			.unwrap();
		assert_eq!(&recv_datagram(&mut dg).payload[..], b"go");
	}

	/// Exercises the full producer -> publisher-encode -> subscriber-decode -> producer seam
	/// (everything but the QUIC datagram send/recv), catching any field-order mismatch between
	/// the wire codec and the model.
	#[tokio::test]
	async fn datagram_wire_roundtrip_between_tracks() {
		use crate::coding::{Decode, Encode};
		use crate::lite;

		let version = lite::Version::Lite05;

		// Origin publishes a datagram; the publisher reads it and encodes the wire body.
		let mut origin = track_producer("test", None);
		let mut origin_dg = origin.subscribe(None);
		let ts = Timestamp::from_millis(7).unwrap();
		let seq = origin.append_datagram(ts, &b"payload"[..]).unwrap();

		let d = recv_datagram(&mut origin_dg);
		let body = lite::Datagram {
			subscribe: 5,
			sequence: d.sequence,
			timestamp: d.timestamp.value(),
			payload: d.payload.clone(),
		}
		.encode_bytes(version)
		.unwrap();

		// Subscriber decodes the body and writes it downstream, preserving the sequence.
		let mut slice = &body[..];
		let wire = lite::Datagram::decode(&mut slice, version).unwrap();
		let mut downstream = track_producer("test", None);
		let mut downstream_dg = downstream.subscribe(None);
		downstream
			.write_datagram(Datagram {
				sequence: wire.sequence,
				timestamp: Timestamp::new(wire.timestamp, Timescale::MILLI).unwrap(),
				payload: wire.payload,
			})
			.unwrap();

		let got = recv_datagram(&mut downstream_dg);
		assert_eq!(got.sequence, seq);
		assert_eq!(got.timestamp, ts);
		assert_eq!(&got.payload[..], b"payload");
	}

	#[tokio::test]
	async fn evict_expired_groups() {
		tokio::time::pause();

		let mut producer = track_producer("test", None);

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
		tokio::time::advance(DEFAULT_LATENCY_MAX + Duration::from_secs(1)).await;

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

		let mut producer = track_producer("test", None);
		producer.append_group().unwrap(); // seq 0

		// Advance time past threshold.
		tokio::time::advance(DEFAULT_LATENCY_MAX + Duration::from_secs(1)).await;

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

		let mut producer = track_producer("test", None);
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

		let mut producer = track_producer("test", None);
		producer.append_group().unwrap(); // seq 0

		let mut consumer = producer.subscribe(None);

		tokio::time::advance(DEFAULT_LATENCY_MAX + Duration::from_secs(1)).await;
		producer.append_group().unwrap(); // seq 1

		// Group 0 was evicted. Consumer should get group 1.
		let group = consumer.assert_group();
		assert_eq!(group.sequence, 1);
	}

	#[tokio::test]
	async fn cache_age_controls_eviction() {
		tokio::time::pause();

		// A shorter cache evicts sooner than the default.
		let mut producer = track_producer("test", Info::default().with_latency_max(Duration::from_secs(1)));
		producer.append_group().unwrap(); // seq 0

		// Past the custom budget but well within DEFAULT_LATENCY_MAX.
		tokio::time::advance(Duration::from_secs(2)).await;
		producer.append_group().unwrap(); // seq 1

		// Seq 0 is gone because the publisher only keeps groups for 1s.
		let state = producer.state.read();
		assert_eq!(live_groups(&state), 1);
		assert_eq!(first_live_sequence(&state), 1);
	}

	#[test]
	fn latency_max_clamped_to_cache() {
		let producer = track_producer("test", Info::default().with_latency_max(Duration::from_secs(2)));

		// A latency budget beyond the cache is capped in the aggregate; a group can't be
		// waited for longer than the publisher keeps it. The subscriber's own preference
		// is stored verbatim, so what it asked for stays readable.
		let mut subscriber = producer.subscribe(Subscription::default().with_latency_max(Duration::from_secs(10)));
		assert_eq!(subscriber.subscription().latency_max, Duration::from_secs(10));
		assert_eq!(producer.subscription().unwrap().latency_max, Duration::from_secs(2));

		// A budget within the cache is left alone, and ZERO (skip immediately) stays ZERO.
		subscriber
			.update(Subscription::default().with_latency_max(Duration::from_millis(500)))
			.unwrap();
		assert_eq!(producer.subscription().unwrap().latency_max, Duration::from_millis(500));

		subscriber
			.update(Subscription::default().with_latency_max(Duration::ZERO))
			.unwrap();
		assert_eq!(producer.subscription().unwrap().latency_max, Duration::ZERO);
	}

	#[test]
	fn latency_max_clamped_via_every_update_path() {
		let producer = track_producer("test", Info::default().with_latency_max(Duration::from_secs(2)));
		let over = Subscription::default().with_latency_max(Duration::from_secs(10));

		// The clamp lives in the aggregation, so it applies no matter which entry point
		// wrote the raw preference. Previously only `Subscriber::update` clamped.
		let mut subscriber = producer.subscribe(over.clone());
		assert_eq!(producer.subscription().unwrap().latency_max, Duration::from_secs(2));

		subscriber.control().update(over.clone()).unwrap();
		assert_eq!(producer.subscription().unwrap().latency_max, Duration::from_secs(2));

		subscriber.update(over).unwrap();
		assert_eq!(producer.subscription().unwrap().latency_max, Duration::from_secs(2));
	}

	#[test]
	fn latency_max_aggregate_clamps_the_max_across_subscribers() {
		let producer = track_producer("test", Info::default().with_latency_max(Duration::from_secs(2)));

		// The aggregate takes the max, then clamps once. Equivalent to clamping each
		// subscriber first, since `min` distributes over `max`.
		let _a = producer.subscribe(Subscription::default().with_latency_max(Duration::from_millis(500)));
		let _b = producer.subscribe(Subscription::default().with_latency_max(Duration::from_secs(10)));

		assert_eq!(producer.subscription().unwrap().latency_max, Duration::from_secs(2));
	}

	#[test]
	fn subscriber_control_updates_while_read_future_is_pending() {
		let producer = track_producer("test", None);
		let mut subscriber = producer.subscribe(None);
		let control = subscriber.control();

		let mut recv = Box::pin(subscriber.recv_group());
		assert!(recv.as_mut().now_or_never().is_none());

		control
			.update(Subscription::default().with_priority(7).with_ordered(false))
			.unwrap();

		let aggregate = producer.subscription().expect("expected an active subscription");
		assert_eq!(aggregate.priority, 7);
		assert!(!aggregate.ordered);
	}

	#[test]
	fn dropped_subscriber_leaves_no_ghost_in_aggregate() {
		// Regression (#2351): a departed subscriber must not keep contributing its
		// last subscription to the aggregate. When it did, a relay's linger loop
		// never observed the track going idle, and an identical viewer reconnecting
		// within the linger window was reset when the stale timer fired.
		let mut producer = track_producer("test", None);
		let a = producer.subscribe(Subscription::default().with_priority(5));

		// Prime the change cursor: the aggregate currently has one subscriber.
		let waiter = kio::Waiter::noop();
		assert!(
			matches!(producer.poll_subscription_changed(&waiter), Poll::Ready(Ok(Some(_)))),
			"one live subscriber should aggregate to Some",
		);

		// The only subscriber leaves.
		drop(a);

		// The aggregate must report the drop to None, not the ghost's last value.
		assert!(
			matches!(producer.poll_subscription_changed(&waiter), Poll::Ready(Ok(None))),
			"a dropped subscriber must not linger in the aggregate",
		);

		// And the snapshot used by the linger loop must agree.
		assert!(
			producer.subscription().is_none(),
			"snapshot must exclude a dropped subscriber",
		);
	}

	#[tokio::test]
	async fn out_of_order_max_sequence_at_front() {
		tokio::time::pause();

		let mut producer = track_producer("test", None);

		// Arrive out of order: seq 5 first, then 3, then 4.
		producer.create_group(group::Info { sequence: 5 }).unwrap();
		producer.create_group(group::Info { sequence: 3 }).unwrap();
		producer.create_group(group::Info { sequence: 4 }).unwrap();

		// max_sequence = 5, which is at the front of the VecDeque.
		{
			let state = producer.state.read();
			assert_eq!(state.max_sequence, Some(5));
		}

		// Expire all three groups.
		tokio::time::advance(DEFAULT_LATENCY_MAX + Duration::from_secs(1)).await;

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

		let mut producer = track_producer("test", None);

		// Arrive: seq 5, then seq 3.
		producer.create_group(group::Info { sequence: 5 }).unwrap();

		tokio::time::advance(DEFAULT_LATENCY_MAX + Duration::from_secs(1)).await;

		// Seq 3 arrives late; max_sequence is still 5 (at front).
		producer.create_group(group::Info { sequence: 3 }).unwrap();

		// Seq 5 is max_sequence (protected). Seq 3 is not expired (just created).
		// Nothing should be evicted.
		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
		}

		// Expire seq 3 as well.
		tokio::time::advance(DEFAULT_LATENCY_MAX + Duration::from_secs(1)).await;

		// Seq 2 arrives late, triggering eviction.
		producer.create_group(group::Info { sequence: 2 }).unwrap();

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

	#[tokio::test]
	async fn abort_clears_cached_groups() {
		let mut producer = track_producer("test", None);
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

	#[tokio::test]
	async fn drop_unfinished_clears_cached_groups() {
		let producer = track_producer("test", None);
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
		let mut producer = track_producer("test", None);
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
		let mut producer = track_producer("test", None);

		// Finishing an empty track is valid (fin = 0, total groups = 0).
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn finish_after_groups() {
		let mut producer = track_producer("test", None);

		producer.append_group().unwrap();
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn finish_at_rejects_a_boundary_at_or_below_the_live_edge() {
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 5 }).unwrap();

		// The boundary is exclusive, so it must be strictly above the highest produced
		// group. 5 or below would orphan groups that already exist.
		assert!(producer.finish_at(4).is_err());
		assert!(producer.finish_at(5).is_err());
		assert!(producer.finish_at(6).is_ok());

		{
			let state = producer.state.read();
			assert_eq!(state.final_sequence, Some(6));
		}

		// Re-finishing is rejected, and no group at or above the boundary can be created.
		assert!(producer.finish_at(6).is_err());
		assert!(producer.create_group(group::Info { sequence: 4 }).is_ok());
		assert!(producer.create_group(group::Info { sequence: 6 }).is_err());
	}

	#[test]
	fn final_sequence_reports_the_declared_boundary() {
		let mut producer = track_producer("test", None);
		assert_eq!(producer.final_sequence(), None);

		producer.create_group(group::Info { sequence: 5 }).unwrap();
		assert_eq!(producer.final_sequence(), None, "a group does not declare a boundary");

		producer.finish_at(9).unwrap();
		assert_eq!(producer.final_sequence(), Some(9));

		// finish() would try to declare a second boundary, so callers check first.
		assert!(producer.finish().is_err());
	}

	#[test]
	fn final_sequence_reports_the_live_edge_after_finish() {
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 5 }).unwrap();
		producer.finish().unwrap();
		assert_eq!(producer.final_sequence(), Some(6));
	}

	#[tokio::test]
	async fn finish_at_declares_a_future_boundary() {
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 5 }).unwrap();

		// Learn the track ends at group 6 (exclusive 7) while the live edge is still 5.
		producer.finish_at(7).unwrap();

		let mut consumer = producer.subscribe(None);
		assert_eq!(consumer.assert_group().sequence, 5);

		// The boundary is known immediately, but the track isn't done: group 6 is still
		// outstanding, so the consumer parks rather than seeing end-of-stream.
		let boundary = consumer
			.finished()
			.now_or_never()
			.expect("boundary is known immediately")
			.expect("would have errored");
		assert_eq!(boundary, 7);
		assert!(
			consumer.recv_group().now_or_never().is_none(),
			"should wait for the outstanding group"
		);

		// The trailing group arrives (below the boundary), then the track completes.
		producer.create_group(group::Info { sequence: 6 }).unwrap();
		assert_eq!(consumer.assert_group().sequence, 6);
		let done = consumer
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track completes once the boundary is reached");
	}

	#[tokio::test]
	async fn recv_group_finishes_without_waiting_for_gaps() {
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 1 }).unwrap();
		producer.finish().unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Seq 5 arrives first.
		producer.create_group(group::Info { sequence: 5 }).unwrap();
		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);

		// Seq 3 arrives late, skipped because 3 <= 5.
		producer.create_group(group::Info { sequence: 3 }).unwrap();
		// Seq 4 arrives late and is also skipped.
		producer.create_group(group::Info { sequence: 4 }).unwrap();
		// Seq 7 arrives and is returned.
		producer.create_group(group::Info { sequence: 7 }).unwrap();

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 7);

		// No more groups. This would block.
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"should block waiting for a higher sequence"
		);
	}

	#[tokio::test]
	async fn next_group_returns_arrivals_in_order() {
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Seq 3 arrives first, then seq 5. Both should be returned in arrival order.
		producer.create_group(group::Info { sequence: 3 }).unwrap();
		producer.create_group(group::Info { sequence: 5 }).unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Out-of-order arrivals: seq 5 first, then seq 3.
		producer.create_group(group::Info { sequence: 5 }).unwrap();
		producer.create_group(group::Info { sequence: 3 }).unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		for s in 0..6 {
			producer.create_group(group::Info { sequence: s }).unwrap();
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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		for s in 0..6 {
			producer.create_group(group::Info { sequence: s }).unwrap();
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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		for s in 0..3 {
			producer.create_group(group::Info { sequence: s }).unwrap();
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
		producer.create_group(group::Info { sequence: 3 }).unwrap();
		producer.create_group(group::Info { sequence: 4 }).unwrap();
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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		consumer.end_at(5);

		// Out-of-order arrivals all within the cap.
		producer.create_group(group::Info { sequence: 2 }).unwrap();
		producer.create_group(group::Info { sequence: 5 }).unwrap();
		producer.create_group(group::Info { sequence: 3 }).unwrap();
		// One beyond the cap; should be held even though it arrived in the middle.
		producer.create_group(group::Info { sequence: 8 }).unwrap();
		producer.create_group(group::Info { sequence: 4 }).unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		producer.write_frame(Timestamp::ZERO, b"hello".as_slice()).unwrap();
		producer.write_frame(Timestamp::ZERO, b"world".as_slice()).unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"hello");

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"world");
	}

	#[tokio::test]
	async fn read_frame_preserves_timestamp() {
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		producer
			.write_frame(Timestamp::from_micros(20_000).unwrap(), b"hello".as_slice())
			.unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(frame.timestamp.as_micros(), 20_000);
		assert_eq!(&frame.payload[..], b"hello");
	}

	#[tokio::test]
	async fn read_frame_skips_stalled_group_for_newer_ready_frame() {
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Seq 3: group open, no frame yet (stalled).
		let _stalled = producer.create_group(group::Info { sequence: 3 }).unwrap();
		// Seq 5: fully-written group with a frame.
		let mut g5 = producer.create_group(group::Info { sequence: 5 }).unwrap();
		g5.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"later"))
			.unwrap();
		g5.finish().unwrap();

		// read_frame should not block on the stalled seq 3. It returns seq 5's frame.
		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block on stalled earlier group")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"later");
	}

	#[tokio::test]
	async fn read_frame_discards_rest_of_multi_frame_group() {
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Group 0 has two frames; only the first is returned.
		let mut g0 = producer.create_group(group::Info { sequence: 0 }).unwrap();
		g0.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"one"))
			.unwrap();
		g0.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"two"))
			.unwrap();
		g0.finish().unwrap();

		// Group 1 is a normal single-frame group.
		producer.write_frame(Timestamp::ZERO, b"next".as_slice()).unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"one");

		// The second frame of group 0 is discarded; the next read jumps to group 1.
		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"next");
	}

	#[tokio::test]
	async fn read_frame_waits_for_pending_group_after_finish() {
		// finish() sets final_sequence, but groups already created with lower sequences
		// can still produce frames. read_frame must not return None prematurely.
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		let mut g0 = producer.create_group(group::Info { sequence: 0 }).unwrap();
		producer.finish().unwrap();

		// Track is finished but group 0 has no frame yet. It must block, not return None.
		assert!(
			consumer.read_frame().now_or_never().is_none(),
			"read_frame must block on a pending group even after finish()"
		);

		// A late frame on the pending group is still delivered.
		g0.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"late"))
			.unwrap();
		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block once a frame is written")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"late");
	}

	#[tokio::test]
	async fn read_frame_respects_start_at() {
		// start_at sets min_sequence; read_frame must skip groups below it even though
		// next_sequence is still 0.
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);
		consumer.start_at(5);

		// Seq 3 has a frame but is below min_sequence, so it must be skipped.
		let mut g3 = producer.create_group(group::Info { sequence: 3 }).unwrap();
		g3.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"skip-me"))
			.unwrap();
		g3.finish().unwrap();

		let mut g5 = producer.create_group(group::Info { sequence: 5 }).unwrap();
		g5.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"keep"))
			.unwrap();
		g5.finish().unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"keep");
	}

	#[tokio::test]
	async fn read_frame_returns_none_when_finished() {
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		producer.write_frame(Timestamp::ZERO, b"only".as_slice()).unwrap();
		producer.finish().unwrap();

		let frame = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(&frame.payload[..], b"only");

		let done = consumer
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none());
	}

	#[test]
	fn append_group_returns_bounds_exceeded_on_sequence_overflow() {
		let mut producer = track_producer("test", None);
		{
			let mut state = producer.state.write().ok().unwrap();
			state.max_sequence = Some(u64::MAX);
		}

		assert!(matches!(producer.append_group(), Err(Error::BoundsExceeded(_))));
	}

	#[tokio::test]
	async fn fetch_cache_hit() {
		let mut producer = track_producer("test", None);

		// Produce a cached group.
		let mut group = producer.append_group().unwrap(); // seq 0
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hello"))
			.unwrap();
		group.finish().unwrap();

		// A cached group resolves immediately and never queues a request. `peek_group`
		// also returns it synchronously.
		let dynamic = producer.dynamic();
		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_some());
		let mut g = consumer.fetch_group(0, None).await.unwrap();
		assert_eq!(g.sequence, 0);
		assert_eq!(&g.read_frame().await.unwrap().unwrap().payload[..], b"hello");

		// Nothing was queued for the dynamic handler to serve.
		assert!(dynamic.poll_requested_group(&kio::Waiter::noop()).is_pending());
	}

	#[tokio::test]
	async fn fetch_miss_signals_dynamic() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		// A cache miss isn't in `peek_group`, but a dynamic handler exists, so
		// `fetch_group` stays pending and queues a request. `*pending` derefs the
		// wrapper to the inner `Fetching` (a `kio::Pollable`).
		assert!(consumer.peek_group(5).is_none());
		let pending = consumer.fetch_group(5, group::Fetch::default().with_priority(7));
		assert!(kio::Pollable::poll(&*pending, &kio::Waiter::noop()).is_pending());

		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
		assert_eq!(req.priority(), 7);

		// Serve it by accepting the request; the fetch then resolves.
		let mut group = req.accept(None).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hi"))
			.unwrap();
		group.finish().unwrap();

		let mut g = pending.await.unwrap();
		assert_eq!(g.sequence, 5);
		assert_eq!(&g.read_frame().await.unwrap().unwrap().payload[..], b"hi");
	}

	#[tokio::test]
	async fn fetch_miss_rejects() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();

		req.reject(Error::Cancel);
		assert!(matches!(pending.await, Err(Error::Cancel)));
		let fetch = producer.state.read().fetch.clone();
		assert!(fetch.read().is_empty());
	}

	#[tokio::test]
	async fn fetch_miss_drop_rejects() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();

		drop(req);
		assert!(matches!(pending.await, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn fetch_reject_does_not_poison_retry() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		req.reject(Error::Cancel);
		assert!(matches!(pending.await, Err(Error::Cancel)));

		let retry = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(None).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"retry"))
			.unwrap();
		group.finish().unwrap();

		let mut group = retry.await.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"retry");
	}

	#[tokio::test]
	async fn fetch_coalesces_concurrent() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		// Two fetches for the same uncached group produce ONE handler request,
		// carrying the higher of the two priorities.
		let first = consumer.fetch_group(5, group::Fetch::default().with_priority(1));
		let second = consumer.fetch_group(5, group::Fetch::default().with_priority(7));
		assert!(kio::Pollable::poll(&*first, &kio::Waiter::noop()).is_pending());

		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
		assert_eq!(req.priority(), 7);
		assert!(
			dynamic.poll_requested_group(&kio::Waiter::noop()).is_pending(),
			"the second fetch queued a duplicate request"
		);

		// A fetch arriving while the request is already in flight joins it too.
		let third = consumer.fetch_group(5, None);

		// One accept resolves all of them.
		let mut group = req.accept(None).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hi"))
			.unwrap();
		group.finish().unwrap();

		assert_eq!(first.await.unwrap().sequence, 5);
		assert_eq!(second.await.unwrap().sequence, 5);
		assert_eq!(third.await.unwrap().sequence, 5);
	}

	#[tokio::test]
	async fn fetch_coalesced_reject_fails_all() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let second = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		req.reject(Error::Cancel);

		assert!(matches!(first.await, Err(Error::Cancel)));
		assert!(matches!(second.await, Err(Error::Cancel)));

		// The rejected attempt is gone: a retry starts a fresh one.
		let retry = consumer.fetch_group(5, None);
		assert!(kio::Pollable::poll(&*retry, &kio::Waiter::noop()).is_pending());
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
	}

	#[tokio::test]
	async fn fetch_queued_fails_when_handlers_leave() {
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		// Queued but never popped: the last handler leaving fails it fast.
		let pending = consumer.fetch_group(5, None);
		assert!(kio::Pollable::poll(&*pending, &kio::Waiter::noop()).is_pending());
		drop(dynamic);
		assert!(matches!(pending.await, Err(Error::NotFound)));

		// And the attempt didn't leak.
		let fetch = producer.state.read().fetch.clone();
		assert!(fetch.read().is_empty());
	}

	#[tokio::test]
	async fn fetch_miss_no_dynamic_not_found() {
		// A track with no `Dynamic` can't serve old content, so a cache miss
		// resolves to NotFound instead of blocking forever.
		let mut producer = track_producer("test", None);
		producer.append_group().unwrap(); // seq 0, but we miss on seq 5
		let consumer = producer.consume();
		assert!(matches!(consumer.fetch_group(5, None).await, Err(Error::NotFound)));
	}

	#[tokio::test]
	async fn fetch_past_final_not_found() {
		let mut producer = track_producer("test", None);
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

	/// Mint a track whose groups register with a bounded [`cache::Pool`].
	fn pooled_producer(capacity: u64) -> (Producer, cache::Pool) {
		let pool = cache::Pool::new(capacity);
		let broadcast = broadcast::Info {
			origin: crate::origin::Info::default().with_pool(pool.clone()),
			..Default::default()
		};
		let producer = Producer::new(Arc::new(broadcast), "test", None);
		(producer, pool)
	}

	fn finished_group(producer: &mut Producer, size: usize) -> u64 {
		let mut group = producer.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; size]))
			.unwrap();
		group.finish().unwrap();
		group.sequence
	}

	#[tokio::test]
	async fn pool_evicts_oldest_group() {
		tokio::time::pause();

		// Fits two 1000-byte groups (plus per-group overhead) but not three.
		let (mut producer, pool) = pooled_producer(3000);

		finished_group(&mut producer, 1000); // seq 0
		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 1
		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 2, pinned as latest

		// The write to seq 2 pushed the pool over budget: seq 0 (stalest, unpinned)
		// was evicted and its bytes released.
		assert!(pool.used() <= 3000, "pool should be back under budget");

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none(), "evicted group is a cache miss");
		assert!(consumer.peek_group(1).is_some());
		assert!(consumer.peek_group(2).is_some());

		// A fresh subscriber skips the evicted group entirely.
		let mut subscriber = producer.subscribe(None);
		assert_eq!(subscriber.assert_group().sequence, 1);
		assert_eq!(subscriber.assert_group().sequence, 2);
	}

	#[tokio::test]
	async fn pool_never_evicts_latest() {
		tokio::time::pause();

		// Far too small for even one group: the latest is pinned and survives anyway.
		let (mut producer, pool) = pooled_producer(100);
		finished_group(&mut producer, 1000);

		assert!(pool.used() > 100, "pinned latest may exceed the budget");
		let mut subscriber = producer.subscribe(None);
		let mut group = subscriber.assert_group();
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.len(), 1000);
	}

	#[tokio::test]
	async fn pool_reads_bump_recency() {
		tokio::time::pause();

		let (mut producer, pool) = pooled_producer(3000);
		let mut subscriber = producer.subscribe(None);

		finished_group(&mut producer, 1000); // seq 0
		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 1
		tokio::time::advance(Duration::from_millis(10)).await;

		// Read seq 0 so seq 1 becomes the least recently used.
		let mut group = subscriber.assert_group();
		assert_eq!(group.sequence, 0);
		group.read_frame().await.unwrap().unwrap();
		tokio::time::advance(Duration::from_millis(10)).await;

		// Over budget: seq 1 (stale) is the victim, the just-read seq 0 survives.
		finished_group(&mut producer, 1000); // seq 2, pinned

		let consumer = producer.consume();
		assert!(
			consumer.peek_group(0).is_some(),
			"recently read group survives: {:?}",
			pool.debug_entries()
		);
		assert!(
			consumer.peek_group(1).is_none(),
			"stale group is evicted: {:?}",
			pool.debug_entries()
		);
	}

	#[tokio::test]
	async fn pool_eviction_aborts_readers() {
		tokio::time::pause();

		let (mut producer, pool) = pooled_producer(3000);
		let mut subscriber = producer.subscribe(None);

		finished_group(&mut producer, 1000); // seq 0
		let group0 = subscriber.assert_group();

		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 1
		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 2 evicts seq 0

		// A consumer holding the evicted group surfaces the eviction, not a hang
		// or a truncated clean end.
		let mut group0 = group0;
		let read = group0.read_frame().await;
		assert!(
			matches!(read, Err(Error::Evicted)),
			"expected Evicted, got {read:?}: {:?}",
			pool.debug_entries()
		);
	}

	#[tokio::test]
	async fn pool_growth_on_old_group_charges() {
		tokio::time::pause();

		let (mut producer, pool) = pooled_producer(3000);

		// Seq 0 stays open (a straggler still being written).
		let mut group0 = producer.append_group().unwrap();
		tokio::time::advance(Duration::from_millis(10)).await;
		// Seq 1 becomes the pinned latest; seq 0 is now evictable.
		let _group1 = producer.append_group().unwrap();
		tokio::time::advance(Duration::from_millis(10)).await;

		// A late frame on the old group still counts against the budget, and can
		// evict that very group once it blows past capacity.
		group0
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 4000]))
			.unwrap();
		assert!(pool.used() <= 3000, "growth on an old group triggers eviction");
		assert!(matches!(group0.abort(Error::Cancel), Err(Error::Evicted)));
	}

	#[tokio::test]
	async fn refetched_latest_group_is_repinned() {
		tokio::time::pause();

		let (mut producer, pool) = pooled_producer(3000);
		let dynamic = producer.dynamic();

		// Seq 0 stays open: the straggler used to apply memory pressure later.
		let mut straggler = producer.append_group().unwrap();
		straggler
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 1000]))
			.unwrap();
		tokio::time::advance(Duration::from_millis(10)).await;

		// The publisher aborts its own latest group; the slot stays at max_sequence.
		let mut latest = producer.append_group().unwrap(); // seq 1
		latest.abort(Error::Cancel).unwrap();
		tokio::time::advance(Duration::from_millis(10)).await;

		// Re-fetch it: the replacement takes over max_sequence and must be
		// re-pinned, or memory pressure could evict the live edge.
		let consumer = producer.consume();
		let pending = consumer.fetch_group(1, None);
		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(None).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 1000]))
			.unwrap();
		group.finish().unwrap();
		pending.await.unwrap();
		tokio::time::advance(Duration::from_millis(10)).await;

		// Blow the budget with the straggler; the refetched latest is pinned, so
		// the straggler itself is the only eligible victim.
		straggler
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 4000]))
			.unwrap();

		assert!(pool.used() <= 3000);
		let mut group = consumer.peek_group(1).expect("refetched latest must stay pinned");
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.len(), 1000);
	}

	#[tokio::test]
	async fn pool_eviction_allows_refetch() {
		tokio::time::pause();

		let (mut producer, _pool) = pooled_producer(3000);
		let dynamic = producer.dynamic();

		finished_group(&mut producer, 1000); // seq 0
		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 1
		tokio::time::advance(Duration::from_millis(10)).await;
		finished_group(&mut producer, 1000); // seq 2 evicts seq 0

		// The evicted group is a miss, so the fetch queues for the dynamic handler
		// (a relay would issue a wire FETCH upstream).
		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none());
		let pending = consumer.fetch_group(0, None);

		let req = dynamic
			.requested_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 0);

		// Accept replaces the evicted slot in place (not Error::Duplicate).
		let mut group = req.accept(None).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"refetched"))
			.unwrap();
		group.finish().unwrap();

		let mut group = pending.await.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"refetched");
	}

	#[tokio::test]
	async fn fetch_aborts_with_track() {
		let mut producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(3, None);
		assert!(kio::Pollable::poll(&*pending, &kio::Waiter::noop()).is_pending());

		producer.abort(Error::Cancel).unwrap();
		assert!(pending.await.is_err());
		drop(dynamic);
	}
}
