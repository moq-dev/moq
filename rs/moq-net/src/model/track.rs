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

use crate::{Error, Result, Subscription, Timescale, Timestamp, coding};
use crate::{broadcast, group};

use super::{Datagram, MAX_DATAGRAM_PAYLOAD};

use std::{
	collections::{HashMap, HashSet, VecDeque},
	sync::Arc,
	task::{Poll, ready},
	time::Duration,
};

/// Default [`Info::cache`] age when the publisher doesn't set one.
pub const DEFAULT_CACHE: Duration = Duration::from_secs(5);

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Info {
	/// Units per second for per-frame timestamps on this track.
	///
	/// Every track is timed; this defaults to [`Timescale::MILLI`]. On Lite05+ it is
	/// reported in TRACK_INFO and the publisher zigzag-delta encodes per-frame
	/// timestamps at this scale on the wire. Protocols whose wire can't carry it
	/// (pre-Lite05 moq-lite, IETF moq-transport) fall back to wall-clock milliseconds.
	#[cfg_attr(feature = "serde", serde(default))]
	pub timescale: Timescale,
	/// How long the publisher keeps old groups available before evicting them
	/// (the newest group is always retained). A subscriber's
	/// [`Subscription::stale`] window is clamped to this, since a group can't be
	/// waited for longer than it's kept around. Reported in TRACK_INFO so
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

#[cfg(feature = "serde")]
fn default_cache() -> Duration {
	DEFAULT_CACHE
}

#[cfg(feature = "serde")]
fn is_default_cache(cache: &Duration) -> bool {
	*cache == DEFAULT_CACHE
}

/// Serialize [`Info::cache`] as a bare integer of milliseconds, matching the
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
impl Default for Info {
	fn default() -> Self {
		Self {
			timescale: Timescale::default(),
			cache: DEFAULT_CACHE,
			priority: 0,
			ordered: true,
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

	/// Set how long old groups stay available before eviction, returning `self` for chaining.
	pub fn with_cache(mut self, cache: Duration) -> Self {
		self.cache = cache;
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

	/// Clamp a subscriber's stale window to this track's [`Self::cache`]: a
	/// subscriber can't wait for a late group longer than the publisher keeps it.
	/// `Duration::ZERO` (skip immediately) is left untouched by the `min`.
	fn clamp_stale(&self, stale: Duration) -> Duration {
		stale.min(self.cache)
	}
}

#[derive(Default)]
struct TrackState {
	// The info for the track; always Some for Subscriber/Producer.
	// A small `Copy` value, inherited by each group it creates.
	info: Option<Info>,

	// Groups in arrival order. `None` entries are tombstones for evicted groups.
	groups: VecDeque<Option<(group::Producer, web_async::time::Instant)>>,

	// Datagrams in arrival order paired with their arrival time, a best-effort send buffer
	// evicted by age (see `MAX_DATAGRAM_AGE`). Shares the group `max_sequence` namespace but
	// is otherwise independent.
	datagrams: VecDeque<(Datagram, web_async::time::Instant)>,

	// Number of datagrams dropped off the front (aged out), mapping a subscriber's absolute
	// cursor to an index into `datagrams` (mirrors `offset` for groups).
	datagram_offset: usize,

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
	// `Dynamic` to serve (see `Dynamic::requested_group`).
	fetches: VecDeque<GroupRequested>,

	// Monotonic IDs for fetches that reached a dynamic handler.
	next_fetch: u64,

	// Per-request failures for popped fetches. Keyed by request ID so rejecting one
	// transient attempt doesn't poison future retries for the same sequence.
	fetch_rejections: HashMap<u64, Error>,

	// Number of live `Dynamic` handles. While zero, the track serves no
	// uncached groups, so a cache-miss `fetch` on an accepted track fails fast
	// instead of blocking forever (mirrors `BroadcastState::dynamic`).
	dynamic: usize,
}

impl TrackState {
	fn poll_info(&self) -> Poll<Result<Info>> {
		if let Some(info) = &self.info {
			Poll::Ready(Ok(*info))
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
		if self.final_sequence.is_some() {
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

	/// Find a cached group by sequence, skipping tombstones. Synchronous, never blocks.
	fn cached_group(&self, sequence: u64) -> Option<group::Consumer> {
		self.groups
			.iter()
			.flatten()
			.find(|(group, _)| group.sequence == sequence)
			.map(|(group, _)| group.consume())
	}

	fn poll_get_group(&self, sequence: u64) -> Poll<Result<Option<group::Consumer>>> {
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
	/// [`Dynamic`] exists to fetch old content (`dynamic == 0`). On-demand tracks
	/// (from a [`Request`]) are dynamic from creation, so a relay's fetch waits to
	/// be served rather than racing the handler into existence.
	fn poll_fetch(&self, sequence: u64, request_id: Option<u64>) -> Poll<Result<group::Consumer>> {
		if let Some(group) = self.cached_group(sequence) {
			return Poll::Ready(Ok(group));
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}

		if let Some(id) = request_id
			&& let Some(err) = self.fetch_rejections.get(&id)
		{
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

	/// Evict groups older than `max_age`, never evicting the max_sequence group.
	///
	/// Groups are in arrival order, so we can stop early when we hit a non-expired,
	/// non-max_sequence group (everything after it arrived even later).
	/// When max_sequence is at the front, we skip past it and tombstone expired groups
	/// behind it.
	fn evict_expired(&mut self, now: web_async::time::Instant, max_age: Duration) {
		for slot in self.groups.iter_mut() {
			let Some((group, created_at)) = slot else { continue };

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

	/// Insert a group fetched for a [`GroupRequest`], setting the track's [`Info`]
	/// if it isn't accepted yet. The group's timescale comes from that info, so a
	/// fetch can serve an as-yet-unaccepted track (e.g. a relay with no live
	/// subscription). The group lands in the cache so a waiting
	/// [`Fetch`] resolves via [`Self::poll_fetch`].
	fn insert_group_request(&mut self, sequence: u64, info: Option<Info>) -> Result<group::Producer> {
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
		let info = *self.info.get_or_insert_with(|| info.unwrap_or_default());

		let group = group::Producer::new(group::Info { sequence }, info);
		let cache = info.cache;
		let now = web_async::time::Instant::now();
		self.max_sequence = Some(self.max_sequence.unwrap_or(0).max(sequence));
		self.groups.push_back(Some((group.clone(), now)));
		self.evict_expired(now, cache);
		Ok(group)
	}

	fn reject_group_request(&mut self, id: u64, err: Error) {
		self.fetch_rejections.entry(id).or_insert(err);
	}

	fn clear_group_request_rejection(&mut self, id: u64) {
		self.fetch_rejections.remove(&id);
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
}

impl Producer {
	/// Build a producer for the given track metadata.
	///
	/// Crate-private: tracks are born from their broadcast via
	/// [`broadcast::Producer::create_track`] (or served on demand through a
	/// [`Request`]), which threads the broadcast's `Arc<broadcast::Info>` down so
	/// the broadcast owns the namespace and there's a single way to mint a track.
	pub(crate) fn new(
		broadcast: Arc<broadcast::Info>,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<Info>>,
	) -> Self {
		let info = info.into().unwrap_or_default();
		Self {
			name: name.into(),
			broadcast,
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
		let track = *info;
		let cache = info.cache;

		let group = group::Producer::new(group, track);
		if !state.duplicates.insert(group.sequence) {
			return Err(Error::Duplicate);
		}

		let now = web_async::time::Instant::now();
		state.max_sequence = Some(state.max_sequence.unwrap_or(0).max(group.sequence));
		state.groups.push_back(Some((group.clone(), now)));
		state.evict_expired(now, cache);

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
		let track = *info;
		let cache = info.cache;

		let group = group::Producer::new(group::Info { sequence }, track);

		let now = web_async::time::Instant::now();
		state.duplicates.insert(sequence);
		state.max_sequence = Some(sequence);
		state.groups.push_back(Some((group.clone(), now)));
		state.evict_expired(now, cache);

		Ok(group)
	}

	/// Append a datagram with the next sequence number, returning the assigned sequence.
	///
	/// A datagram is delivered best-effort over a single QUIC datagram, parallel to the
	/// track's groups but drawing from the same sequence namespace (so interleaving with
	/// [`Self::append_group`] never reuses a number). The payload must not exceed
	/// [`MAX_DATAGRAM_PAYLOAD`]; there is no group fallback. An origin publisher uses this;
	/// a relay preserving upstream numbering uses [`Self::write_datagram`].
	pub fn append_datagram<B: Into<bytes::Bytes>>(&mut self, timestamp: Timestamp, payload: B) -> Result<u64> {
		let payload = payload.into();
		if payload.len() > MAX_DATAGRAM_PAYLOAD {
			return Err(Error::WrongSize);
		}
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
	/// relay can forward a datagram without renumbering it. The payload must not exceed
	/// [`MAX_DATAGRAM_PAYLOAD`]. Most origin publishers want [`Self::append_datagram`] instead.
	pub fn write_datagram(&mut self, mut datagram: Datagram) -> Result<()> {
		if datagram.payload.len() > MAX_DATAGRAM_PAYLOAD {
			return Err(Error::WrongSize);
		}
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
		state.push_datagram(datagram);
		Ok(())
	}

	/// Create a group with a single frame, at the given presentation timestamp.
	///
	/// The timestamp is converted into the track's timescale. Use
	/// [`Self::write_frame_now`] to stamp wall-clock time instead.
	pub fn write_frame<B: crate::IntoBytes>(&mut self, timestamp: Timestamp, frame: B) -> Result<()> {
		let mut group = self.append_group()?;
		group.write_frame(timestamp, frame)?;
		group.finish()?;
		Ok(())
	}

	/// Like [`Self::write_frame`] but stamps the frame with wall-clock now
	/// ([`Timestamp::now`]).
	pub fn write_frame_now<B: crate::IntoBytes>(&mut self, frame: B) -> Result<()> {
		let mut group = self.append_group()?;
		group.write_frame_now(frame)?;
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
		Consumer {
			name: self.name.clone(),
			state: self.state.consume(),
		}
	}

	/// Subscribe to this in-process track, resolving synchronously.
	///
	/// The info is fixed at creation, so there's nothing to wait for (no
	/// SUBSCRIBE_OK round trip). The subscriber's stale window is clamped to the
	/// track's cache. Pass `None` for [`Subscription::default`].
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> Subscriber {
		let mut preferences = subscription.into().unwrap_or_default();

		let mut state = self.modify().expect("track producer state is never closed");
		let info = *state.info.as_ref().expect("producer always has info");
		preferences.stale = info.clamp_stale(preferences.stale);
		let subscription = kio::Producer::new(preferences);
		state.subscriptions.push(subscription.consume());
		drop(state);

		Subscriber {
			name: self.name.clone(),
			info,
			state: self.state.consume(),
			subscription,
			index: 0,
			datagram_index: 0,
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

/// Pop the next queued group fetch off the shared state and wrap it in a
/// [`GroupRequest`] bound to a fresh producer handle. Shared by every
/// [`Dynamic`] handle on the track.
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
		id: req.id,
		sequence: req.sequence,
		priority: req.priority,
		done: false,
	}))
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
	state: kio::Producer<TrackState>,
}

impl Dynamic {
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

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Bump `dynamic` so each live handle is counted (mirrors `broadcast::Dynamic`).
		if let Ok(mut state) = self.state.write() {
			state.dynamic += 1;
		}
		Self {
			name: self.name.clone(),
			state: self.state.clone(),
		}
	}
}

impl Drop for Dynamic {
	fn drop(&mut self) {
		// Unlike `broadcast::Dynamic`, dropping the last handle doesn't abort the track:
		// a live `Producer` may still be serving the subscription. It just stops
		// fetch serving, after which an accepted track's cache miss fails fast.
		if let Ok(mut state) = self.state.write() {
			state.dynamic = state.dynamic.saturating_sub(1);
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
		if let Ok(mut state) = self.state.write()
			&& state.final_sequence.is_none()
		{
			state.groups.clear();
			state.datagrams.clear();
			state.duplicates.clear();
		}
	}
}

/// Aggregate every live subscriber's preferences into the most demanding request.
///
/// Read-only: iterates `subscriptions` immutably and registers `waiter` on each, so it
/// never flags the [`TrackState`] as modified. Marking it modified would drain and wake
/// unrelated waiters on the channel (e.g. a [`Subscribe`] parked on track info),
/// which races with [`Request::accept`] and can drop that wakeup. Callers decide
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

	pub fn consume(&self) -> Consumer {
		Consumer {
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
/// Obtained from [`Producer::demand`]. A publisher uses it to react to
/// whether anyone is subscribed (on-demand capture / encoding) without being able
/// to publish frames or close the track. It's a weak handle, so it neither keeps
/// the track alive nor pins its cached groups; once the owning [`Producer`]
/// goes away, [`used`](Self::used) / [`unused`](Self::unused) report the track's
/// closure.
#[derive(Clone)]
pub struct Demand {
	name: Arc<str>,
	state: kio::Weak<TrackState>,
}

impl Demand {
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
/// Obtained from [`broadcast::Consumer::track`]. Holding it sends nothing
/// to the publisher; it just names a track you can [`subscribe`](Self::subscribe)
/// to (a live, ongoing stream of groups) later. The same handle can be subscribed
/// to multiple times, and clones are cheap.
#[derive(Clone)]
pub struct Consumer {
	name: Arc<str>,
	state: kio::Consumer<TrackState>,
}

impl Consumer {
	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Open a live subscription.
	///
	/// Registers the subscription on the track and returns a [`kio::Pending`] that resolves to the
	/// [`Subscriber`] once the track info is available, or the track's abort error (or
	/// [`Error::Dropped`]) if it is already closed.
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> kio::Pending<Subscribe> {
		let subscription = kio::Producer::new(subscription.into().unwrap_or_default());

		// Register the subscription if the track is live. If it is already closed, the returned
		// future resolves to the abort error via `Subscribe::poll_ok`.
		if let Ok(mut state) = self.state.write() {
			state.subscriptions.push(subscription.consume());
		}

		kio::Pending::new(Subscribe {
			name: self.name.clone(),
			state: self.state.clone(),
			subscription,
		})
	}

	/// Return a cached group by sequence without blocking, or `None` if it isn't in
	/// the cache. Use [`Self::fetch_group`] to wait for a group that a [`Dynamic`]
	/// will serve on demand.
	pub fn get_group(&self, sequence: u64) -> Option<group::Consumer> {
		self.state.read().cached_group(sequence)
	}

	/// Fetch a single past group, without holding a live subscription.
	///
	/// Returns a [`kio::Pending`] that resolves to the [`group::Consumer`]:
	/// immediately if the group is cached, otherwise once a [`Dynamic`] serves
	/// the request (a wire FETCH for a relay). `options` accepts `None`, a [`group::Fetch`],
	/// or `group::Fetch::default()`.
	///
	/// The returned future resolves to [`Error::NotFound`] when the group can never be served
	/// (past the final sequence, or no [`Dynamic`] on the track), or the track's abort error
	/// if it's already closed.
	pub fn fetch_group(&self, sequence: u64, options: impl Into<Option<group::Fetch>>) -> kio::Pending<Fetch> {
		let options = options.into().unwrap_or_default();
		let mut request_id = None;

		// Queue a request only when a handler can serve it but the group isn't cached yet. A cached
		// group, an unservable sequence (NotFound), or a closed track all resolve through
		// `Fetch::poll` without a queue entry.
		if let Ok(mut state) = self.state.write() {
			if state.poll_fetch(sequence, None).is_pending() {
				let id = state.next_fetch;
				state.next_fetch = state.next_fetch.wrapping_add(1);
				state.fetches.push_back(GroupRequested {
					id,
					sequence,
					priority: options.priority,
				});
				request_id = Some(id);
			}
		}

		kio::Pending::new(Fetch {
			state: self.state.clone(),
			sequence,
			request_id,
		})
	}

	pub fn info(&self) -> kio::Pending<InfoQuery> {
		kio::Pending::new(InfoQuery {
			state: self.state.clone(),
		})
	}
}

/// The pollable state of a [`Consumer::subscribe`]; awaited via the
/// [`kio::Pending`] wrapper, whose `DerefMut` exposes [`Self::update`].
pub struct Subscribe {
	name: Arc<str>,
	state: kio::Consumer<TrackState>,
	subscription: kio::Producer<Subscription>,
}

impl Subscribe {
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Subscriber>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;

		Poll::Ready(Ok(Subscriber {
			name: self.name.clone(),
			info,
			state: self.state.clone(),
			subscription: self.subscription.clone(),
			index: 0,
			datagram_index: 0,
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

impl kio::Future for Subscribe {
	type Output = Result<Subscriber>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// The pollable state of a [`Consumer::info`]; awaited via the
/// [`kio::Pending`] wrapper.
pub struct InfoQuery {
	state: kio::Consumer<TrackState>,
}

impl InfoQuery {
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Info>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;
		Poll::Ready(Ok(info))
	}
}

impl kio::Future for InfoQuery {
	type Output = Result<Info>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// A specific group requested via [`Consumer::fetch_group`], queued on the
/// track for a [`Dynamic`] to serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GroupRequested {
	/// The request ID matching the waiting [`Fetch`].
	id: u64,
	/// The group sequence the consumer wants.
	sequence: u64,
	/// The requested delivery priority.
	priority: u8,
}

/// A consumer's request for a single past group, handed to a handler via
/// [`Dynamic::requested_group`].
///
/// The handler fulfills it by calling [`Self::accept`], which inserts the group
/// into the track cache (resolving the matching [`Consumer::fetch_group`]) and
/// returns a [`group::Producer`] to fill. A relay typically opens a wire FETCH, reads
/// FETCH_OK, then accepts. The request carries its own producer handle, so it works
/// the same whether or not the track has been accepted yet.
pub struct GroupRequest {
	state: kio::Producer<TrackState>,
	id: u64,
	sequence: u64,
	priority: u8,
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
		TrackState::modify(&self.state)?.insert_group_request(self.sequence, info.into())
	}

	/// Reject the fetch, resolving the waiting [`Consumer::fetch_group`] with `err`.
	pub fn reject(mut self, err: Error) {
		self.done = true;
		if let Ok(mut state) = self.state.write() {
			state.reject_group_request(self.id, err);
		}
	}
}

impl Drop for GroupRequest {
	fn drop(&mut self) {
		if self.done {
			return;
		}
		if let Ok(mut state) = self.state.write() {
			state.reject_group_request(self.id, Error::Dropped);
		}
	}
}

/// The pollable state of a [`Consumer::fetch_group`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the
/// [`group::Consumer`] once the group lands in the track's cache (already present,
/// or produced after a wire FETCH), or [`Error::NotFound`] if it can never exist.
pub struct Fetch {
	state: kio::Consumer<TrackState>,
	sequence: u64,
	request_id: Option<u64>,
}

impl kio::Future for Fetch {
	type Output = Result<group::Consumer>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		// `poll_fetch` already yields a `Result<group::Consumer>` (group, or NotFound /
		// abort); the outer error is the channel closing without one.
		let res = match ready!(
			self.state
				.poll(waiter, |state| state.poll_fetch(self.sequence, self.request_id))
		) {
			Ok(res) => res,
			Err(closed) => return Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped))),
		};

		if let Some(id) = self.request_id
			&& let Ok(mut state) = self.state.write()
		{
			state.clear_group_request_rejection(id);
		}

		Poll::Ready(res)
	}
}

/// A live subscription to a track, used to read its groups.
///
/// Created via [`Consumer::subscribe`](Consumer::subscribe), or
/// directly from a [`Producer`] for an in-process track. Carries this
/// subscriber's [`Subscription`] preferences, which feed the producer's aggregate.
pub struct Subscriber {
	name: Arc<str>,
	info: Info,
	state: kio::Consumer<TrackState>,

	subscription: kio::Producer<Subscription>,
	/// Arrival-order cursor used by [`Self::recv_group`].
	index: usize,
	/// Arrival-order cursor used by [`Self::recv_datagram`], independent of groups.
	datagram_index: usize,
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

impl Subscriber {
	pub fn info(&self) -> &Info {
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
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
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
		let Some((datagram, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv_datagram(self.datagram_index))?)
		else {
			return Poll::Ready(Ok(None));
		};

		self.datagram_index = found_index + 1;
		self.next_sequence = self.next_sequence.max(datagram.sequence.saturating_add(1));
		Poll::Ready(Ok(Some(datagram)))
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
	pub async fn next_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_next_group(waiter)).await
	}

	/// A helper that calls [`Self::poll_next_group`] and returns its first frame,
	/// skipping the rest of the group. Intended for single-frame groups (see
	/// [`Producer::write_frame`]).
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

	/// Poll for the group with the given sequence.
	///
	/// This waits for live arrival, not on-demand retrieval. If the sequence is
	/// below the final sequence but was already evicted from the cache, this parks
	/// until the track closes. Use [`Consumer::fetch_group`] for a past group that a
	/// [`Dynamic`] can serve on demand.
	pub fn poll_get_group(&self, waiter: &kio::Waiter, sequence: u64) -> Poll<Result<Option<group::Consumer>>> {
		self.poll(waiter, |state| state.poll_get_group(sequence))
	}

	/// Wait until the group with the given sequence becomes available.
	///
	/// Resolves to `Some(group::Consumer)` once the group is in the cache.
	/// Resolves to `None` only when `sequence` is at or past the track's
	/// `final_sequence` (set by `finish()` / `finish_at()`), since such a
	/// group can never be produced. Sequences below `final_sequence` still
	/// wait, since older groups may still arrive out of order. If the sequence
	/// was already evicted, this waits until the track closes; use
	/// [`Consumer::fetch_group`] for on-demand retrieval of past groups.
	pub async fn get_group(&self, sequence: u64) -> Result<Option<group::Consumer>> {
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
}

impl Request {
	pub(crate) fn new(broadcast: Arc<broadcast::Info>, name: impl Into<Arc<str>>) -> Self {
		let name = name.into();
		let state = kio::Producer::<TrackState>::default();
		let dynamic = Dynamic::new(name.clone(), state.clone());
		Self {
			name,
			broadcast,
			state,
			prev_subscription: None,
			_dynamic: dynamic,
		}
	}

	/// The requested track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	pub fn consume(&self) -> Consumer {
		Consumer {
			name: self.name.clone(),
			state: self.state.consume(),
		}
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
	/// The track's name must match [`Self::name`]. Returns [`Error::NotFound`] on
	/// mismatch, or the broadcast's abort error if it closed while pending.
	pub fn accept(self, info: impl Into<Option<Info>>) -> Producer {
		self.state.write().ok().unwrap().info = Some(info.into().unwrap_or_default());
		Producer {
			name: self.name,
			broadcast: self.broadcast,
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
			Err(_) => unreachable!("a Request holds the only producer"),
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
		let big = bytes::Bytes::from(vec![0u8; MAX_DATAGRAM_PAYLOAD + 1]);
		let ts = Timestamp::from_millis(0).unwrap();
		assert!(matches!(
			producer.append_datagram(ts, big.clone()),
			Err(Error::WrongSize)
		));
		assert!(matches!(
			producer.write_datagram(Datagram {
				sequence: 0,
				timestamp: ts,
				payload: big,
			}),
			Err(Error::WrongSize)
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

		let mut producer = track_producer("test", None);
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
		let mut producer = track_producer("test", Info::default().with_cache(Duration::from_secs(1)));
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
		let producer = track_producer("test", Info::default().with_cache(Duration::from_secs(2)));

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

		let mut producer = track_producer("test", None);

		// Arrive: seq 5, then seq 3.
		producer.create_group(group::Info { sequence: 5 }).unwrap();

		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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
		tokio::time::advance(DEFAULT_CACHE + Duration::from_secs(1)).await;

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
	fn insert_finish_validates_sequence_and_freezes_to_max() {
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 5 }).unwrap();

		assert!(producer.finish_at(4).is_err());
		assert!(producer.finish_at(10).is_err());
		assert!(producer.finish_at(5).is_ok());

		{
			let state = producer.state.read();
			assert_eq!(state.final_sequence, Some(6));
		}

		assert!(producer.finish_at(5).is_err());
		assert!(producer.create_group(group::Info { sequence: 4 }).is_ok());
		assert!(producer.create_group(group::Info { sequence: 5 }).is_err());
	}

	#[tokio::test]
	async fn recv_group_finishes_without_waiting_for_gaps() {
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 1 }).unwrap();
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

		// Seq 3 arrives late — skipped because 3 <= 5.
		producer.create_group(group::Info { sequence: 3 }).unwrap();
		// Seq 4 arrives late — also skipped.
		producer.create_group(group::Info { sequence: 4 }).unwrap();
		// Seq 7 arrives — returned.
		producer.create_group(group::Info { sequence: 7 }).unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Seq 3 arrives first, then seq 5 — both should be returned in arrival order.
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

		producer.write_frame_now(b"hello".as_slice()).unwrap();
		producer.write_frame_now(b"world".as_slice()).unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Seq 3: group open, no frame yet (stalled).
		let _stalled = producer.create_group(group::Info { sequence: 3 }).unwrap();
		// Seq 5: fully-written group with a frame.
		let mut g5 = producer.create_group(group::Info { sequence: 5 }).unwrap();
		g5.write_frame_now(bytes::Bytes::from_static(b"later")).unwrap();
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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		// Group 0 has two frames; only the first is returned.
		let mut g0 = producer.create_group(group::Info { sequence: 0 }).unwrap();
		g0.write_frame_now(bytes::Bytes::from_static(b"one")).unwrap();
		g0.write_frame_now(bytes::Bytes::from_static(b"two")).unwrap();
		g0.finish().unwrap();

		// Group 1 is a normal single-frame group.
		producer.write_frame_now(b"next".as_slice()).unwrap();

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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		let mut g0 = producer.create_group(group::Info { sequence: 0 }).unwrap();
		producer.finish().unwrap();

		// Track is finished but group 0 has no frame yet — must block, not return None.
		assert!(
			consumer.read_frame().now_or_never().is_none(),
			"read_frame must block on a pending group even after finish()"
		);

		// A late frame on the pending group is still delivered.
		g0.write_frame_now(bytes::Bytes::from_static(b"late")).unwrap();
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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);
		consumer.start_at(5);

		// Seq 3 has a frame but is below min_sequence — must be skipped.
		let mut g3 = producer.create_group(group::Info { sequence: 3 }).unwrap();
		g3.write_frame_now(bytes::Bytes::from_static(b"skip-me")).unwrap();
		g3.finish().unwrap();

		let mut g5 = producer.create_group(group::Info { sequence: 5 }).unwrap();
		g5.write_frame_now(bytes::Bytes::from_static(b"keep")).unwrap();
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
		let mut producer = track_producer("test", None);
		let mut consumer = producer.subscribe(None);

		producer.write_frame_now(b"only".as_slice()).unwrap();
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
		let mut producer = track_producer("test", None);
		producer.create_group(group::Info { sequence: 1 }).unwrap();
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
		group.write_frame_now(bytes::Bytes::from_static(b"hello")).unwrap();
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
		let producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		// A cache miss isn't in `get_group`, but a dynamic handler exists, so
		// `fetch_group` stays pending and queues a request. `*pending` derefs the
		// wrapper to the inner `Fetch` (a `kio::Future`).
		assert!(consumer.get_group(5).is_none());
		let pending = consumer.fetch_group(5, group::Fetch::default().with_priority(7));
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
		group.write_frame_now(bytes::Bytes::from_static(b"hi")).unwrap();
		group.finish().unwrap();

		let mut g = pending.await.unwrap();
		assert_eq!(g.sequence, 5);
		assert_eq!(&g.read_frame().await.unwrap().unwrap()[..], b"hi");
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
		assert!(producer.state.read().fetch_rejections.is_empty());
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
		group.write_frame_now(bytes::Bytes::from_static(b"retry")).unwrap();
		group.finish().unwrap();

		let mut group = retry.await.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap()[..], b"retry");
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

	#[tokio::test]
	async fn fetch_aborts_with_track() {
		let mut producer = track_producer("test", None);
		let dynamic = producer.dynamic();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(3, None);
		assert!(kio::Future::poll(&*pending, &kio::Waiter::noop()).is_pending());

		producer.abort(Error::Cancel).unwrap();
		assert!(pending.await.is_err());
		drop(dynamic);
	}
}
