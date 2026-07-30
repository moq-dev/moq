//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once ([Producer::write_frame]), or in chunks
//! ([Producer::create_frame]).
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! Frames are numbered from 0 in write order. A group can be short at its front or its
//! back but never in the middle: [Producer::start_at] starts it later, so a handle can
//! carry the tail of a group whose leading frames came from somewhere else, and
//! [Producer::finish] ends it wherever writing stopped. [Consumer::start_at] /
//! [Consumer::end_at] bound a reader to a sub-range the same way [`track::Subscriber`]
//! bounds group sequences.
//!
//! The stream is closed with [Error] when all writers or readers are dropped.
use crate::cache;
use crate::frame::{self, Frame, FrameBuf};
use crate::{Timescale, stats, track};
use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::task::{Poll, ready};

use crate::{Error, IntoBytes, Result, Timestamp};

/// Maximum total size of frames cached in a group before old frames are evicted.
///
/// Doubles as the per-frame size cap: a single frame can be at most this large (a
/// larger declared size is refused before allocating), so one maximum-size frame can
/// fill a group's cache.
const MAX_GROUP_CACHE: u64 = 32 * 1024 * 1024; // 32 MB

/// A group contains a sequence number because they can arrive out of order.
///
/// You can use [track::Producer::append_group] if you just want to +1 the sequence number.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct Info {
	/// Per-track sequence number used to detect ordering and gaps. Higher numbers
	/// supersede lower ones; consumers may skip late arrivals.
	pub sequence: u64,
}

impl Info {
	/// Create an untimed producer for this group.
	///
	/// Test-only: real groups are created via [`track::Producer`], which
	/// supplies the parent track's [`track::Info`]. This helper exists for in-crate
	/// tests that don't exercise timestamps.
	#[cfg(test)]
	pub(crate) fn produce(self) -> Producer {
		Producer::new(self, track::Info::default(), Default::default())
	}
}

impl From<usize> for Info {
	fn from(sequence: usize) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

impl From<u64> for Info {
	fn from(sequence: u64) -> Self {
		Self { sequence }
	}
}

impl From<u32> for Info {
	fn from(sequence: u32) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

impl From<u16> for Info {
	fn from(sequence: u16) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

/// The in-flight (tail) frame being written. At most one exists at a time, since a
/// group is a single ordered stream.
pub(crate) struct Partial {
	timestamp: Timestamp,
	buf: FrameBuf,
}

/// Shared group state. `pub(crate)` so [`frame`] handles can observe the abort flag
/// while streaming a partial frame.
#[derive(Default)]
pub(crate) struct GroupState {
	// Completed frames, each a contiguous payload. Evicted frames are popped from the
	// front; `offset` tracks how many.
	pub(crate) frames: VecDeque<Frame>,

	// The single in-flight frame, if one is open.
	pub(crate) partial: Option<Partial>,

	// Index of the first frame this handle holds: frames evicted from the front, plus any
	// the group deliberately started past (see [`Producer::start_at`]). Reading below it
	// is [`Error::Lagged`] either way; the frames are not here.
	pub(crate) offset: usize,

	// The index the next frame written will get. Tracked separately from `frames` so it
	// survives the cache being released: a route taking the track over needs to know where
	// production stopped, and an abort is exactly when it asks.
	next_index: usize,

	// One past the last frame that was fully written. Trails `next_index` while a chunked
	// frame is in flight, which is the frame a replacement route has to redeliver: only
	// its opener saw the payload, and only partly.
	committed: usize,

	// The total size (in bytes) of all cached frames plus any in-flight frame.
	pub(crate) cache: u64,

	// Mirrors `cache` into the track's shared cache pool, so the group's bytes count
	// against the byte budget tracks evict toward.
	charge: cache::Charge,

	// Once finalized, the total number of frames the group will ever contain. Recorded
	// at finish so the count outlives an abort that clears the cache.
	pub(crate) fin: Option<usize>,

	// The error that caused the group to be aborted, if any.
	pub(crate) abort: Option<Error>,
}

impl GroupState {
	/// Resolve the source for the frame at `index`: a completed frame (whole) or the
	/// in-flight tail (streamed). Used by [`Consumer::poll_next_frame`].
	fn poll_frame_source(&self, index: usize) -> Poll<Result<Option<(frame::Info, frame::Source)>>> {
		if index < self.offset {
			return Poll::Ready(Err(Error::Lagged));
		}
		let local = index - self.offset;
		if let Some(f) = self.frames.get(local) {
			let info = frame::Info {
				size: f.payload.len() as u64,
				timestamp: f.timestamp,
			};
			return Poll::Ready(Ok(Some((info, frame::Source::Complete(f.payload.clone())))));
		}
		if local == self.frames.len()
			&& let Some(p) = &self.partial
		{
			let info = frame::Info {
				size: p.buf.capacity() as u64,
				timestamp: p.timestamp,
			};
			return Poll::Ready(Ok(Some((info, frame::Source::Partial(p.buf.clone())))));
		}
		ready!(self.poll_terminal(index))?;
		Poll::Ready(Ok(None))
	}

	/// Resolve the group's terminal state for a reader positioned at `index`.
	///
	/// A finished group is still aborted once its frames are released to free memory
	/// (aged out of the track's latency window, or evicted by the cache pool). A reader
	/// that already consumed every frame is missing nothing, so it gets the clean end of
	/// group; one that fell short sees the abort rather than a silently truncated stream.
	fn poll_terminal(&self, index: usize) -> Poll<Result<()>> {
		match (self.fin, &self.abort) {
			(Some(total), Some(err)) if index < total => Poll::Ready(Err(err.clone())),
			(Some(_), _) => Poll::Ready(Ok(())),
			(None, Some(err)) => Poll::Ready(Err(err.clone())),
			(None, None) => Poll::Pending,
		}
	}

	fn poll_finished(&self) -> Poll<Result<u64>> {
		// The count is recorded at finish, so a later abort that cleared the cache
		// doesn't turn a complete group into an error.
		if let Some(total) = self.fin {
			Poll::Ready(Ok(total as u64))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Evict completed frames from the front until within the byte budget.
	fn evict(&mut self) {
		while self.cache > MAX_GROUP_CACHE {
			let Some(frame) = self.frames.pop_front() else {
				break;
			};
			let size = frame.payload.len() as u64;
			self.cache -= size;
			self.charge.sub(size);
			self.offset += 1;
		}
	}

	/// Drop the cached frames (and any in-flight tail) and release their pool charge.
	fn release(&mut self) {
		self.frames.clear();
		self.partial = None;
		self.cache = 0;
		self.charge.clear();
	}
}

fn modify(state: &kio::Producer<GroupState>) -> Result<kio::Mut<'_, GroupState>> {
	state.write().map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
}

/// Writes frames to a group in order.
///
/// Each group is delivered independently over a QUIC stream.
/// Use [Self::write_frame] for simple single-buffer frames,
/// or [Self::create_frame] for multi-chunk streaming writes.
pub struct Producer {
	// Mutable stream state.
	state: kio::Producer<GroupState>,

	// The group header containing the sequence number. A small `Copy` value,
	// inherited by each frame (see [`Self::create_frame`]).
	info: Info,

	// The parent track's properties, inherited rather than passed piecemeal. Its
	// `timescale` is used by [`Self::create_frame`] to normalize every frame's
	// timestamp into the track scale before it enters the stream. Threaded down by
	// value from [`track::Producer::create_group`] / `append_group`.
	track: track::Info,

	// The parent track's account against the shared cache pool. Held here as well as
	// in the group's `cache::Charge` so a frame write can settle the track's eviction
	// debt with the group lock released.
	cache: Arc<cache::Track>,

	// Ingress payload meter, set by a tagged [`track::Producer`] via
	// [`Self::with_meter`]. Empty (no-op) for an untagged group.
	stats: stats::Meter,

	// Shared by every clone: its `Drop` is the abrupt-teardown, running exactly once
	// when the last of them goes.
	alive: Arc<Alive>,
}

/// Ends the group when the last [`Producer`] clone drops, including the clone the
/// parent track holds in its cache.
///
/// A refcount rather than a "am I the last one?" check inside `Drop`: that answer is
/// a snapshot, and acting on it is exactly what can invalidate it. Holding a producer
/// of its own also keeps the state writable until the teardown has run, whatever order
/// the last owner's fields drop in.
struct Alive {
	info: Info,
	state: kio::Producer<GroupState>,
}

impl Drop for Alive {
	fn drop(&mut self) {
		// See track::Alive: the last producer dropping without a clean finish releases
		// the cached frames so a stale consumer can't pin their buffers forever. A
		// finished group keeps its cache so consumers can drain.
		if let Ok(mut state) = modify(&self.state)
			&& state.fin.is_none()
		{
			// Dropped without finish() or abort(), so consumers will see
			// Error::Dropped mid-group. Deliberate ends go through finish()/abort().
			tracing::warn!(
				sequence = self.info.sequence,
				"group::Producer dropped without finish() or abort()"
			);
			state.release();
		}
	}
}

impl std::ops::Deref for Producer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Producer {
	/// Create a group producer bound to its parent track's [`track::Info`] and cache
	/// account.
	///
	/// Crate-private: groups are only constructed via [`track::Producer`], which
	/// threads both down so properties like the timescale are inherited rather than
	/// passed in. Every frame added to this group is normalized to the track's
	/// timescale by [`Self::create_frame`].
	///
	/// Charges the group into `cache`, so its cached bytes count against the budget the
	/// track evicts toward under memory pressure.
	pub(crate) fn new(info: Info, track: track::Info, cache: Arc<cache::Track>) -> Self {
		let state = kio::Producer::<GroupState>::default();
		state.write().ok().expect("a new group is open").charge = cache.charge();
		let alive = Arc::new(Alive {
			info,
			state: state.clone(),
		});
		Self {
			info,
			state,
			track,
			cache,
			stats: stats::Meter::default(),
			alive,
		}
	}

	/// Attach an ingress payload meter, counting this as one delivered group.
	/// Called by a tagged [`track::Producer`] when it creates the group.
	pub(crate) fn with_meter(mut self, meter: stats::Meter) -> Self {
		meter.group();
		self.stats = meter;
		self
	}

	/// The group header.
	pub(crate) fn info(&self) -> Info {
		self.info
	}

	/// The parent track's timescale.
	pub fn timescale(&self) -> Timescale {
		self.track.timescale
	}

	/// Start the group at frame `index` rather than 0, so the first frame written lands
	/// there.
	///
	/// A group can be short at its front or its back, never in the middle: this trims
	/// the front, and simply stopping (then [`finish`](Self::finish)ing) trims the back.
	/// The frames below `index` are not a gap this handle will ever fill, so a reader
	/// positioned below it gets [`Error::Lagged`], the same as one that fell behind an
	/// eviction. They belong to whoever produced the head of the group, typically
	/// another route serving the same track (see [`crate::track::Subscriber`]).
	///
	/// The counterpart of [`Consumer::start_at`], which positions a *reader* the same
	/// way. Where the group begins is part of its shape, so this must come before the
	/// first frame; afterwards it returns [`Error::Closed`].
	pub fn start_at(&mut self, index: u64) -> Result<()> {
		let index = usize::try_from(index).map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		if index == usize::MAX {
			return Err(Error::BoundsExceeded(crate::coding::BoundsExceeded));
		}

		let mut state = modify(&self.state)?;
		// Every write advances `next_index` past `offset`, so this is "nothing written
		// yet" in a way a front eviction can't fake.
		if state.fin.is_some() || state.next_index != state.offset {
			return Err(Error::Closed);
		}
		state.offset = index;
		state.next_index = index;
		state.committed = index;
		Ok(())
	}

	/// A helper method to write a frame from a single byte buffer.
	///
	/// If you want to write multiple chunks, use [Self::create_frame] to get a frame producer.
	/// But an upfront size is required.
	///
	/// `timestamp` is converted into the parent track's timescale. For data without
	/// a presentation time, pass [`Timestamp::now`] explicitly.
	pub fn write_frame<B: IntoBytes>(&mut self, timestamp: Timestamp, data: B) -> Result<()> {
		let timestamp = timestamp
			.convert(self.track.timescale)
			.map_err(|_| Error::TimestampMismatch)?;
		let payload = data.into_bytes();
		if payload.len() as u64 > MAX_GROUP_CACHE {
			return Err(Error::FrameTooLarge);
		}

		let mut state = modify(&self.state)?;
		if state.fin.is_some() {
			return Err(Error::Closed);
		}
		let next_index = state
			.next_index
			.checked_add(1)
			.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let size = payload.len() as u64;
		state.cache += size;
		state.charge.add(size);
		state.frames.push_back(Frame { timestamp, payload });
		state.next_index = next_index;
		state.committed = state.next_index;
		state.evict();
		drop(state);

		// With the group lock released (lock order is track then group), settle
		// eviction debt if enough has been written since the track last paid.
		self.cache.settle();

		// Ingress payload: one whole frame written.
		self.stats.frames(1);
		self.stats.bytes(size);
		Ok(())
	}

	/// Create a frame with an upfront size and presentation timestamp, streamed in
	/// chunks. Borrows the group exclusively until the returned [`frame::Producer`]
	/// is finished or dropped, so only one frame is open at a time.
	///
	/// The `timestamp` is converted into the parent track's timescale, so the scale you
	/// build it with doesn't have to match the track. Returns [`Error::FrameTooLarge`]
	/// if the declared size exceeds the group's byte budget (refused before allocating)
	/// or [`Error::TimestampMismatch`] if the timestamp can't be converted (overflow).
	pub fn create_frame(&mut self, frame: frame::Info) -> Result<frame::Producer<'_>> {
		let timestamp = frame
			.timestamp
			.convert(self.track.timescale)
			.map_err(|_| Error::TimestampMismatch)?;
		if frame.size > MAX_GROUP_CACHE {
			return Err(Error::FrameTooLarge);
		}
		let buf = FrameBuf::new(frame.size as usize);

		let mut state = modify(&self.state)?;
		if state.fin.is_some() {
			return Err(Error::Closed);
		}
		let next_index = state
			.next_index
			.checked_add(1)
			.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
		debug_assert!(state.partial.is_none(), "a frame is already open");
		state.cache += frame.size;
		state.charge.add(frame.size);
		state.partial = Some(Partial {
			timestamp,
			buf: buf.clone(),
		});
		state.next_index = next_index;
		state.evict();
		drop(state);

		// With the group lock released (lock order is track then group), settle
		// eviction debt if enough has been written since the track last paid.
		self.cache.settle();

		// Ingress payload: one frame opened; its bytes are counted per chunk as the
		// frame::Producer writes them.
		self.stats.frames(1);
		let meter = self.stats.clone();

		let info = frame::Info {
			size: frame.size,
			timestamp,
		};
		Ok(frame::Producer::new(self, buf, info).with_meter(meter))
	}

	/// Wake consumers parked on the group channel (called after a partial write).
	pub(crate) fn frame_notify(&self) {
		// Taking the write lock and dropping it triggers kio's notify.
		let _ = self.state.write();
	}

	/// Commit the in-flight frame as a completed frame (called by [`frame::Producer::finish`]).
	pub(crate) fn frame_commit(&mut self, frame: Frame) -> Result<()> {
		let mut state = modify(&self.state)?;
		// Bytes were already counted against the cache (and the pool charge) when the
		// frame was created; committing just moves the tail into the completed set.
		state.partial = None;
		state.frames.push_back(frame);
		state.committed = state.next_index;
		Ok(())
	}

	/// Fail the group because an in-flight frame couldn't complete (called by
	/// [`frame::Producer::abort`] / its drop).
	pub(crate) fn frame_abort(&mut self, err: Error) {
		let _ = self.clone().abort(err);
	}

	/// One past the index of the last frame written (completed or in-flight), which is
	/// also the index the next frame will get.
	///
	/// Counts any frames the group [started past](Self::start_at) or evicted, so it's the
	/// group's logical length rather than the number of frames this handle holds.
	pub fn frame_count(&self) -> usize {
		self.state.read().next_index
	}

	/// Mark the group as complete; no more frames will be written.
	///
	/// Borrows rather than consumes, so a later failure can still be reported through
	/// [`abort`](Self::abort). The handle also keeps the cached frames readable.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = modify(&self.state)?;
		state.fin = Some(state.next_index);
		Ok(())
	}

	/// Abort the group with the given error.
	///
	/// Consumes the handle. Drops the cached frames so a stale [`Consumer`] can't pin
	/// their buffers in memory forever; consumers that haven't drained yet surface the
	/// abort error instead of the leftover cache.
	pub fn abort(self, err: Error) -> Result<()> {
		let mut guard = modify(&self.state)?;
		guard.abort = Some(err);
		guard.release();
		guard.close();
		Ok(())
	}

	/// Whether the group has been aborted (including pool eviction). The track's
	/// read paths treat an aborted cached group as absent.
	pub(crate) fn is_aborted(&self) -> bool {
		self.state.read().abort.is_some()
	}

	/// Whether the group was cleanly finished, so no further frame can be appended.
	pub(crate) fn is_finished(&self) -> bool {
		self.state.read().fin.is_some()
	}

	/// The index of the first frame this group still holds: what a reader positioned
	/// below it would be [`Error::Lagged`] on. Non-zero when the group started later
	/// (see [`Self::start_at`]) or its head was evicted.
	pub(crate) fn first_frame(&self) -> usize {
		self.state.read().offset
	}

	/// One past the last frame that was fully written.
	///
	/// Trails [`Self::frame_count`] while a chunked frame is open, and stops there if
	/// that frame never completes. This is the boundary a replacement route resumes
	/// from: an incomplete frame has to be redelivered whole, since a reader can only
	/// use it whole.
	pub(crate) fn committed_frames(&self) -> usize {
		self.state.read().committed
	}

	/// The group's full cached footprint (payload plus fixed overhead), used by the
	/// track to size this group as an eviction victim.
	pub(crate) fn cache_size(&self) -> u64 {
		self.state.read().charge.size()
	}

	/// Tick of the group's last cache access, driving eviction protection and age
	/// expiry (see [`cache::Pool::average`]).
	pub(crate) fn cache_accessed(&self) -> u64 {
		self.state.read().charge.accessed()
	}

	/// Enter the group into the evictable population: demoted from the live edge,
	/// or inserted behind it. Idempotent; a no-op once the group is closed.
	pub(crate) fn cache_demote(&self) {
		if let Ok(mut state) = self.state.write() {
			state.charge.demote();
		}
	}

	/// Record a cache access (a FETCH hit, or a fetched backfill's birth),
	/// protecting the group from eviction and restarting its expiry clock. A no-op
	/// once the group is closed.
	pub(crate) fn cache_refresh(&self) {
		if let Ok(mut state) = self.state.write() {
			state.charge.refresh();
		}
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info,
			track: self.track.clone(),
			inner: ConsumerKind::Plain(Plain {
				state: self.state.consume(),
				index: 0,
				end: None,
				prefetch: Prefetch::default(),
			}),
			// Untagged: a tagged track attaches the egress meter via `with_meter`
			// when it hands the consumer to a subscriber/fetch.
			stats: stats::Meter::default(),
		}
	}

	/// Block until the group is closed or aborted.
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the group is closed or aborted; ready with the cause.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.state.poll_closed(waiter).map(|()| self.abort_reason())
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await.map_err(|_| self.abort_reason())
	}

	/// The recorded abort reason, or [`Error::Dropped`] if the group closed without one.
	fn abort_reason(&self) -> Error {
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

impl Clone for Producer {
	fn clone(&self) -> Self {
		Self {
			info: self.info,
			state: self.state.clone(),
			track: self.track.clone(),
			cache: self.cache.clone(),
			stats: self.stats.clone(),
			alive: self.alive.clone(),
		}
	}
}

/// A small inline batch of completed frames, drained from the shared group state
/// under one lock and then handed out without re-locking.
///
/// Each [`Consumer::read_frame`] otherwise takes the group mutex and allocates a
/// waker just to clone one `Bytes`; draining a batch amortizes both across `CAP`
/// frames. Storage is inline and uninitialized (no heap), so a consumer that never
/// reads whole frames, or drains through a higher-level buffer, pays nothing.
struct Prefetch {
	// Initialized, not-yet-taken frames are `frames[pos..len]`; the rest are uninitialized.
	frames: [MaybeUninit<Frame>; Self::CAP],
	pos: usize,
	len: usize,
}

impl Prefetch {
	const CAP: usize = 8;

	/// Take the next buffered frame, or `None` if the batch is drained.
	fn pop(&mut self) -> Option<Frame> {
		if self.pos == self.len {
			return None;
		}
		// SAFETY: `pos < len`, so this slot was written by `fill` and not yet taken.
		let frame = unsafe { self.frames[self.pos].assume_init_read() };
		self.pos += 1;
		Some(frame)
	}

	/// Refill with up to `CAP` frames. Must be drained first (`pop` returned `None`).
	fn fill(&mut self, frames: impl Iterator<Item = Frame>) {
		debug_assert_eq!(self.pos, self.len, "fill on a non-empty batch would leak frames");
		self.pos = 0;
		self.len = 0;
		for frame in frames.take(Self::CAP) {
			self.frames[self.len].write(frame);
			self.len += 1;
		}
	}

	/// `(frame count, total payload bytes)` of the buffered, not-yet-taken frames.
	/// Read once per fill to bump the egress payload counters for the whole batch.
	fn buffered(&self) -> (u64, u64) {
		let mut bytes = 0u64;
		for slot in &self.frames[self.pos..self.len] {
			// SAFETY: slots in `pos..len` are initialized (written by `fill`, not yet popped).
			bytes += unsafe { slot.assume_init_ref() }.payload.len() as u64;
		}
		((self.len - self.pos) as u64, bytes)
	}
}

impl Default for Prefetch {
	fn default() -> Self {
		Self {
			frames: [const { MaybeUninit::uninit() }; Self::CAP],
			pos: 0,
			len: 0,
		}
	}
}

impl Drop for Prefetch {
	fn drop(&mut self) {
		for slot in &mut self.frames[self.pos..self.len] {
			// SAFETY: slots in `pos..len` are initialized and were never taken.
			unsafe { slot.assume_init_drop() };
		}
	}
}

/// Consume a group, frame-by-frame.
///
/// Usually a view of one [`Producer`], but a group served across a route change is
/// *spliced*: it reads each contributing route's copy in turn, joined at the frame the
/// takeover happened on, so the reader never sees the seam.
pub struct Consumer {
	inner: ConsumerKind,

	// Immutable stream state.
	info: Info,

	// The parent track's info, inherited from the producer. Its `timescale` lets the
	// wire publisher emit per-frame timestamps at the right scale for a fetched group.
	track: track::Info,

	// Egress payload meter, set by a tagged track via [`Self::with_meter`]. Empty
	// (no-op) for an untagged group.
	stats: stats::Meter,
}

// `Plain` is the hot path and carries an inline frame prefetch, so boxing it to even the
// variants out would cost an allocation per group to save a pointer chase on the rare one.
#[expect(clippy::large_enum_variant)]
enum ConsumerKind {
	Plain(Plain),
	// Boxed: the spliced cursor set dwarfs the plain one, and splicing is the rare case.
	Spliced(Box<super::resume::Group>),
}

/// The cursor state for a group backed by a single [`Producer`].
struct Plain {
	// Shared state with the producer.
	state: kio::Consumer<GroupState>,

	// The index of the next frame to read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,

	// Inclusive cap on `index`, set by [`Consumer::end_at`]. Reads end cleanly past it.
	end: Option<usize>,

	// A batch of completed frames drained ahead under one lock (whole-frame reads only).
	prefetch: Prefetch,
}

impl Clone for Plain {
	fn clone(&self) -> Self {
		// A clone shares the channel and inherits `index`, but starts with an empty
		// prefetch: it re-reads its batch from the shared state, in parallel.
		Self {
			state: self.state.clone(),
			index: self.index,
			end: self.end,
			prefetch: Prefetch::default(),
		}
	}
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		Self {
			inner: match &self.inner {
				ConsumerKind::Plain(plain) => ConsumerKind::Plain(plain.clone()),
				ConsumerKind::Spliced(spliced) => ConsumerKind::Spliced(Box::new((**spliced).clone())),
			},
			info: self.info,
			track: self.track.clone(),
			// Inherit the meter without re-counting the group: the original already
			// counted it when the track handed it out.
			stats: self.stats.clone(),
		}
	}
}

impl std::ops::Deref for Consumer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Consumer {
	/// Rebuild this consumer as the head of a group assembled across route changes,
	/// keeping the group's identity and its track's properties. See [`super::resume`].
	pub(crate) fn into_spliced(self, spliced: super::resume::Group) -> Self {
		Self {
			inner: ConsumerKind::Spliced(Box::new(spliced)),
			info: self.info,
			track: self.track,
			stats: self.stats,
		}
	}

	/// Attach an egress payload meter, counting this as one delivered group.
	/// Called by a tagged track when it hands the consumer to a subscriber or fetch.
	pub(crate) fn with_meter(mut self, meter: stats::Meter) -> Self {
		meter.group();
		self.stats = meter;
		self
	}

	/// The parent track's timescale.
	pub fn timescale(&self) -> Timescale {
		self.track.timescale
	}

	/// The index of the next frame this consumer will return.
	///
	/// Starts at 0, or at the group's first available frame once [`Self::start_at`] has
	/// clamped it, and advances by one per frame read.
	pub fn index(&self) -> u64 {
		match &self.inner {
			ConsumerKind::Plain(plain) => plain.index as u64,
			ConsumerKind::Spliced(spliced) => spliced.index(),
		}
	}

	/// Skip ahead so the next frame returned is `index`, discarding anything buffered
	/// below it.
	///
	/// Clamped *up* to the group's first available frame: frames the group never held
	/// (see [`Producer::start_at`]) or has since evicted can't be returned, so asking
	/// for one just starts at the first that exists. Read [`Self::index`] back to learn
	/// where the cursor actually landed.
	/// Only moves forward; a lower `index` is ignored, since the frames behind the
	/// cursor may already have been handed out.
	pub fn start_at(&mut self, index: u64) {
		match &mut self.inner {
			ConsumerKind::Plain(plain) => plain.start_at(index),
			ConsumerKind::Spliced(spliced) => spliced.start_at(index),
		}
	}

	/// Stop after frame `index` (inclusive), or remove the cap.
	///
	/// Reads past the cap end cleanly (`None`), as if the group finished there. Unlike
	/// [`Self::start_at`] this can move in either direction: raising it re-offers frames
	/// that are still cached.
	pub fn end_at(&mut self, index: impl Into<Option<u64>>) {
		let index = index.into();
		match &mut self.inner {
			ConsumerKind::Plain(plain) => {
				plain.end = index.map(|index| usize::try_from(index).unwrap_or(usize::MAX));
			}
			ConsumerKind::Spliced(spliced) => spliced.end_at(index),
		}
	}

	/// Return a consumer for the next frame for chunked reading.
	pub async fn next_frame(&mut self) -> Result<Option<frame::Consumer>> {
		kio::wait(|waiter| self.poll_next_frame(waiter)).await
	}

	/// Poll for the next frame, without blocking.
	///
	/// Returns None if the group is finished and the index is out of range, or the cursor
	/// passed the [`Self::end_at`] cap.
	pub fn poll_next_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Consumer>>> {
		let stats = self.stats.clone();
		match &mut self.inner {
			ConsumerKind::Plain(plain) => plain.poll_next_frame(waiter, &stats),
			ConsumerKind::Spliced(spliced) => {
				// The per-route copies underneath are untagged, so meter the spliced
				// stream here: it is the one the subscriber actually reads.
				let res = ready!(spliced.poll_next_frame(waiter))?;
				if res.is_some() {
					stats.frames(1);
				}
				Poll::Ready(Ok(res.map(|frame| frame.with_meter(stats))))
			}
		}
	}

	/// Read the next frame (timestamp and payload) all at once, without blocking.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		let stats = self.stats.clone();
		match &mut self.inner {
			ConsumerKind::Plain(plain) => plain.poll_read_frame(waiter, &stats),
			ConsumerKind::Spliced(spliced) => {
				let res = ready!(spliced.poll_read_frame(waiter))?;
				if let Some(frame) = &res {
					stats.frames(1);
					stats.bytes(frame.payload.len() as u64);
				}
				Poll::Ready(Ok(res))
			}
		}
	}

	/// Read the next frame (timestamp and payload) all at once.
	pub async fn read_frame(&mut self) -> Result<Option<frame::Frame>> {
		if let ConsumerKind::Plain(plain) = &mut self.inner {
			// Serve from the prefetched batch without building a future or allocating a waker.
			if !plain.capped()
				&& let Some(frame) = plain.prefetch.pop()
			{
				plain.index += 1;
				return Ok(Some(frame));
			}
		}
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Poll for the final number of frames in the group.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		match &mut self.inner {
			ConsumerKind::Plain(plain) => plain.poll(waiter, |state| state.poll_finished()),
			ConsumerKind::Spliced(spliced) => spliced.poll_finished(waiter),
		}
	}

	/// Block until the group is finished, returning the number of frames in the group.
	pub async fn finished(&mut self) -> Result<u64> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}
}

impl Plain {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&kio::Ref<'_, GroupState>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Whether the cursor has passed the `end_at` cap.
	fn capped(&self) -> bool {
		self.end.is_some_and(|end| self.index > end)
	}

	fn start_at(&mut self, index: u64) {
		let index = usize::try_from(index).unwrap_or(usize::MAX);
		let index = index.max(self.state.read().offset);
		if index <= self.index {
			return;
		}
		self.index = index;
		// The batch was drained from below the new cursor, so it can't be reused.
		self.prefetch = Prefetch::default();
	}

	fn poll_next_frame(&mut self, waiter: &kio::Waiter, stats: &stats::Meter) -> Poll<Result<Option<frame::Consumer>>> {
		if self.capped() {
			return Poll::Ready(Ok(None));
		}

		// Hand out any frames a prior read_frame prefetched before touching the tail.
		// Their bytes were already counted at the batch fill, so the frame::Consumer
		// carries no meter.
		if let Some(frame) = self.prefetch.pop() {
			self.index += 1;
			let info = frame::Info {
				size: frame.payload.len() as u64,
				timestamp: frame.timestamp,
			};
			let source = frame::Source::Complete(frame.payload);
			return Poll::Ready(Ok(Some(frame::Consumer::new(self.state.clone(), info, source))));
		}

		let index = self.index;
		let Some((info, source)) = ready!(self.poll(waiter, |state| state.poll_frame_source(index))?) else {
			return Poll::Ready(Ok(None));
		};

		self.index += 1;
		// A direct read (not prefetched): count the frame here; the frame::Consumer
		// counts its bytes per chunk as they're read out.
		stats.frames(1);
		Poll::Ready(Ok(Some(
			frame::Consumer::new(self.state.clone(), info, source).with_meter(stats.clone()),
		)))
	}

	fn poll_read_frame(&mut self, waiter: &kio::Waiter, stats: &stats::Meter) -> Poll<Result<Option<frame::Frame>>> {
		if self.capped() {
			return Poll::Ready(Ok(None));
		}

		// Fast path: serve from the prefetched batch without locking or allocating a waker.
		if let Some(frame) = self.prefetch.pop() {
			self.index += 1;
			return Poll::Ready(Ok(Some(frame)));
		}

		// The batch is drained: refill it under a single lock, registering the waiter if
		// nothing is ready. Borrow the two fields disjointly so the closure can fill.
		let index = self.index;
		// Never buffer past the cap: `end_at` can be raised later, and those frames must
		// come from the shared state then, not from a batch drained under the old cap.
		let budget = self.end.map_or(usize::MAX, |end| (end - index).saturating_add(1));
		let prefetch = &mut self.prefetch;
		let res = self.state.poll(waiter, |state| {
			if index < state.offset {
				return Poll::Ready(Err(Error::Lagged));
			}
			// `local` can run past the buffered count when frames were cleared or evicted out
			// from under us (abort, unfinished drop, an eviction gap); clamp so `range` never
			// panics on an out-of-bounds start. `fill` always resets the batch, so an empty
			// range leaves `len == 0` and the terminal checks below resolve abort/fin/pending.
			let local = (index - state.offset).min(state.frames.len());
			prefetch.fill(state.frames.range(local..).take(budget).cloned());
			if prefetch.len > 0 {
				return Poll::Ready(Ok(()));
			}
			// Nothing completed at `index`: an in-flight tail waits, otherwise resolve
			// the terminal state (whole-frame reads never stream the partial).
			state.poll_terminal(index)
		});

		match ready!(res) {
			Ok(Ok(())) => {}
			Ok(Err(err)) => return Poll::Ready(Err(err)),
			Err(state) => return Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
		}

		// A fresh batch was just filled (empty only on a clean end). Count the whole
		// batch once here, under no lock, so the drained pops that follow stay free.
		let (frames, bytes) = self.prefetch.buffered();
		stats.frames(frames);
		stats.bytes(bytes);

		Poll::Ready(Ok(self.prefetch.pop().inspect(|_| {
			self.index += 1;
		})))
	}
}

/// Options for a one-shot [`track::Consumer::fetch_group`] of a past group.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Fetch {
	/// Delivery priority for the fetched group's stream. Defaults to 0.
	pub priority: u8,

	/// Index of the first frame to fetch within the group. Defaults to 0, the whole group.
	///
	/// Use this to fill a hole left by a route change: the group's head is already
	/// cached locally and only the tail is missing.
	///
	/// There is no matching end: a fetch always runs to the end of the group, and a
	/// caller wanting less caps the returned consumer with [`Consumer::end_at`]. Stopping
	/// the *fetch* short would put a group in the cache that is indistinguishable from a
	/// complete one, so a later fetch of the whole group would resolve from it and come
	/// up short.
	pub frame_start: u64,
}

impl Fetch {
	/// Set the delivery priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: u8) -> Self {
		self.priority = priority;
		self
	}

	/// Set the first frame to fetch, returning `self` for chaining.
	pub fn with_frame_start(mut self, frame_start: u64) -> Self {
		self.frame_start = frame_start;
		self
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use bytes::Bytes;
	use futures::FutureExt;

	#[test]
	fn basic_frame_reading() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"frame0"))
			.unwrap();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"frame1"))
			.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let f0 = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.size, 6);
		let f1 = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f1.size, 6);
		let end = consumer.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	#[test]
	fn read_frame_all_at_once() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"hello"))
			.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"hello"));
	}

	#[test]
	fn read_frame_preserves_timestamp() {
		let mut producer = Info { sequence: 0 }.produce();
		let timestamp = Timestamp::from_micros(20_000).unwrap();
		producer.write_frame(timestamp, Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.timestamp.as_micros(), 20_000);
		assert_eq!(frame.payload, Bytes::from_static(b"hello"));
	}

	#[test]
	fn chunked_frame_reads_whole() {
		let mut producer = Info { sequence: 0 }.produce();
		{
			let mut frame = producer
				.create_frame(frame::Info {
					size: 10,
					timestamp: Timestamp::ZERO,
				})
				.unwrap();
			frame.write(Bytes::from_static(b"hello")).unwrap();
			frame.write(Bytes::from_static(b"world")).unwrap();
			frame.finish().unwrap();
		}
		producer.finish().unwrap();

		// Frame data is held in a single per-frame buffer; a whole-frame read returns
		// the full contents in one slice.
		let mut consumer = producer.consume();
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"helloworld"));
	}

	#[test]
	fn chunked_frame_streams_partial() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		let mut frame = producer
			.create_frame(frame::Info {
				size: 6,
				timestamp: Timestamp::ZERO,
			})
			.unwrap();
		frame.write(Bytes::from_static(b"foo")).unwrap();

		// A consumer can stream the in-flight tail before it's finished.
		let mut f = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		let c1 = f.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c1, Some(Bytes::from_static(b"foo")));
		assert!(f.read_chunk().now_or_never().is_none());

		frame.write(Bytes::from_static(b"bar")).unwrap();
		frame.finish().unwrap();

		let c2 = f.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c2, Some(Bytes::from_static(b"bar")));
		let c3 = f.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c3, None);
	}

	#[test]
	fn group_finish_returns_none() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let end = consumer.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	#[test]
	fn abort_propagates() {
		let producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		producer.abort(crate::Error::Cancel).unwrap();

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Cancel)));
	}

	#[test]
	fn abort_clears_cached_frames() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();

		// A stale consumer that never reads must not pin the cached frames.
		let _consumer = producer.consume();
		assert_eq!(producer.state.read().frames.len(), 1);

		producer.clone().abort(crate::Error::Cancel).unwrap();

		let state = producer.state.read();
		assert!(state.frames.is_empty(), "cached frames should be dropped on abort");
		assert_eq!(state.cache, 0);
	}

	#[test]
	fn drop_unfinished_clears_cached_frames() {
		let producer = Info { sequence: 0 }.produce();
		let mut writer = producer.clone();
		writer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();

		// A stale consumer keeps the channel (and thus the cache) alive.
		let mut consumer = producer.consume();
		assert_eq!(producer.state.read().frames.len(), 1);

		// Drop every producer without finishing: the cache is released.
		drop(writer);
		drop(producer);

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Dropped)));
	}

	#[test]
	fn drop_finished_keeps_cached_frames() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		drop(producer);

		// A cleanly finished group keeps its cache so the consumer can still drain.
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"data"));
	}

	#[tokio::test]
	async fn pending_then_ready() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		// Consumer blocks because no frames yet.
		assert!(consumer.next_frame().now_or_never().is_none());

		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
			.unwrap();
		producer.finish().unwrap();

		let frame = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.size, 4);
	}

	#[test]
	fn eviction_drops_old_frames() {
		let mut producer = Info { sequence: 0 }.produce();

		// Write frames that total more than MAX_GROUP_CACHE.
		let big = Bytes::from(vec![0u8; MAX_GROUP_CACHE as usize]);
		producer.write_frame(Timestamp::ZERO, big.clone()).unwrap();
		producer.write_frame(Timestamp::ZERO, big).unwrap();

		// The first frame should have been evicted (tombstoned via offset).
		let state = producer.state.read();
		assert_eq!(state.offset, 1);
		assert_eq!(state.frames.len(), 1);
		assert_eq!(state.frames[0].payload.len(), MAX_GROUP_CACHE as usize);
	}

	#[test]
	fn next_frame_returns_cache_full_on_tombstone() {
		let mut producer = Info { sequence: 0 }.produce();

		let big = Bytes::from(vec![0u8; MAX_GROUP_CACHE as usize]);
		producer.write_frame(Timestamp::ZERO, big.clone()).unwrap();
		producer.write_frame(Timestamp::ZERO, big).unwrap();

		let mut consumer = producer.consume();
		// First frame was evicted, next_frame should return Lagged.
		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Lagged)));
	}

	#[test]
	fn no_eviction_under_budget() {
		let mut producer = Info { sequence: 0 }.produce();
		// Many small frames stay cached: there is no frame-count cap, only a byte budget.
		for _ in 0..100_000 {
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		}
		producer.finish().unwrap();

		let state = producer.state.read();
		assert_eq!(state.offset, 0);
		assert_eq!(state.frames.len(), 100_000);
	}

	#[test]
	fn clone_consumer_independent() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();

		let mut c1 = producer.consume();
		// Read one frame from c1
		let _ = c1.next_frame().now_or_never().unwrap().unwrap().unwrap();

		// Clone c1, inheriting its index (past first frame).
		let mut c2 = c1.clone();

		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();
		producer.finish().unwrap();

		// c2 should get the second frame (inherited index)
		let f = c2.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f.size, 1); // "b"

		let end = c2.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	/// Reading more than one prefetch batch drains every frame in order across the
	/// batch boundary (the refill starts exactly where the previous batch ended).
	#[test]
	fn read_frame_crosses_prefetch_batches() {
		let n = Prefetch::CAP * 3 + 5;
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..n {
			producer
				.write_frame(Timestamp::ZERO, Bytes::from(vec![i as u8; 4]))
				.unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		for i in 0..n {
			let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload, Bytes::from(vec![i as u8; 4]));
		}
		assert!(consumer.read_frame().now_or_never().unwrap().unwrap().is_none());
	}

	/// A finished group is still aborted once its frames are released to free memory (the
	/// track's latency window, or the cache pool). A reader that already drained every frame
	/// is missing nothing, so it must see the clean end of group rather than the abort.
	#[test]
	fn abort_after_finish_keeps_the_clean_end_for_a_drained_reader() {
		let mut producer = Info { sequence: 0 }.produce();
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"hello"))
			.unwrap();
		producer.finish().unwrap();

		let mut drained = producer.consume();
		let mut behind = producer.consume();
		let frame = drained.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"hello"));

		producer.abort(Error::Old).unwrap();

		// Drained everything before the abort: nothing is missing.
		assert!(drained.read_frame().now_or_never().unwrap().unwrap().is_none());
		assert!(drained.next_frame().now_or_never().unwrap().unwrap().is_none());

		// Never read the frame, and its bytes are gone: a truncated stream, not a clean end.
		assert!(matches!(behind.read_frame().now_or_never().unwrap(), Err(Error::Old)));
	}

	/// The frame count is fixed at finish, so an abort that clears the cache can't turn a
	/// complete group into an error.
	#[test]
	fn finished_survives_a_later_abort() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		producer.abort(Error::Old).unwrap();

		assert_eq!(consumer.finished().now_or_never().unwrap().unwrap(), 2);
	}

	/// `next_frame` drains frames a prior `read_frame` prefetched, preserving order.
	#[test]
	fn interleave_read_and_next_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..5u8 {
			producer.write_frame(Timestamp::ZERO, Bytes::from(vec![i; 1])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		// The first whole-frame read prefetches all five frames into the batch.
		let f0 = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.payload, Bytes::from(vec![0u8; 1]));

		// next_frame must continue from the batch, not skip ahead or repeat.
		for i in 1..5u8 {
			let mut f = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
			let data = f.read_all().now_or_never().unwrap().unwrap();
			assert_eq!(data, Bytes::from(vec![i; 1]));
		}
		assert!(consumer.next_frame().now_or_never().unwrap().unwrap().is_none());
	}

	/// A `read_frame` whose index sits past the buffered frames (cleared by an abort, or an
	/// eviction gap) must surface the error, not panic on an out-of-range `range(local..)`.
	#[test]
	fn read_frame_past_cleared_frames_does_not_panic() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"b")).unwrap();

		let mut consumer = producer.consume();
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();

		// Abort clears the cached frames but leaves the consumer's index (2) past them, so the
		// refill's `local` (2) exceeds `frames.len()` (0).
		producer.abort(Error::Cancel).unwrap();

		let result = consumer.read_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(Error::Cancel)), "expected Cancel, got {result:?}");
	}

	/// Dropping a consumer mid-batch must drop the buffered-but-untaken frames
	/// (exercises the `MaybeUninit` Drop path; run under miri to catch leaks/UB).
	#[test]
	fn drop_with_partial_batch() {
		let mut producer = Info { sequence: 0 }.produce();
		for _ in 0..Prefetch::CAP {
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		// Take one frame so the batch is filled but only partially drained.
		let _ = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		drop(consumer);
	}

	/// A frame whose timestamp is at a different scale is converted to the group's
	/// scale by `create_frame`.
	#[test]
	fn create_frame_converts_mismatched_scale() {
		use crate::{Timescale, Timestamp};

		let mut producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
			Default::default(),
		);
		let frame = frame::Info {
			size: 3,
			timestamp: Timestamp::from_millis(1).unwrap(), // 1ms -> 1000µs
		};
		let writer = producer.create_frame(frame).unwrap();
		assert_eq!(writer.timestamp.scale(), Timescale::MICRO);
		assert_eq!(writer.timestamp.value(), 1000);
	}

	/// An explicit current timestamp is converted to the group's scale.
	#[tokio::test]
	async fn create_frame_converts_current_timestamp() {
		use crate::Timescale;

		let mut producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
			Default::default(),
		);
		let writer = producer
			.create_frame(frame::Info {
				size: 3,
				timestamp: Timestamp::now(),
			})
			.unwrap();
		assert_eq!(writer.timestamp.scale(), Timescale::MICRO);
		assert!(!writer.timestamp.is_zero(), "local clock should be non-zero");
	}

	/// A group can start partway in, so a route can serve the tail of a group whose
	/// head came from somewhere else.
	#[test]
	fn start_at_starts_the_group_later() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.start_at(3).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"d")).unwrap();
		producer.finish().unwrap();

		// The frame landed at index 3, so the group's length counts the missing head.
		assert_eq!(producer.frame_count(), 4);

		let mut consumer = producer.consume();
		assert_eq!(consumer.finished().now_or_never().unwrap().unwrap(), 4);

		// A reader positioned at the start is missing the head, exactly like one that
		// fell behind an eviction.
		assert!(matches!(
			consumer.read_frame().now_or_never().unwrap(),
			Err(Error::Lagged)
		));
	}

	/// Seeking to the group's first available frame is how a spliced reader picks up
	/// the tail; a lower index clamps up rather than failing.
	#[test]
	fn start_at_clamps_up_to_the_first_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.start_at(3).unwrap();
		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"d")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		consumer.start_at(1);
		assert_eq!(consumer.index(), 3, "clamped up to the first frame that exists");
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
			Bytes::from_static(b"d")
		);
	}

	/// `end_at` ends the read cleanly at the cap, and raising it re-offers the frames
	/// still cached behind it.
	#[test]
	fn end_at_caps_and_reopens() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..4u8 {
			producer.write_frame(Timestamp::ZERO, Bytes::from(vec![i])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		consumer.end_at(1);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			0
		);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			1
		);
		assert!(
			consumer.read_frame().now_or_never().unwrap().unwrap().is_none(),
			"capped reads end cleanly"
		);

		consumer.end_at(None);
		assert_eq!(
			consumer.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			2
		);
	}

	/// Where the group begins is part of its shape, so it can't move once frames exist.
	#[test]
	fn start_at_rejected_after_a_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		// Re-declaring before the first frame is fine; the shape isn't committed yet.
		producer.start_at(2).unwrap();
		producer.start_at(3).unwrap();

		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"a")).unwrap();
		assert!(matches!(producer.start_at(4), Err(Error::Closed)));
		assert_eq!(producer.frame_count(), 4, "the frame landed at index 3");

		// Finishing likewise settles the shape.
		let mut producer = Info { sequence: 1 }.produce();
		producer.finish().unwrap();
		assert!(matches!(producer.start_at(1), Err(Error::Closed)));
	}

	/// The start must leave room for at least one frame index.
	#[test]
	fn start_at_rejects_the_largest_index() {
		let mut producer = Info { sequence: 0 }.produce();
		assert!(matches!(
			producer.start_at(usize::MAX as u64),
			Err(Error::BoundsExceeded(_))
		));
	}

	/// The per-frame size cap (the group byte budget) is enforced before allocating.
	#[test]
	fn create_frame_rejects_oversized() {
		let mut producer = Info { sequence: 0 }.produce();
		let result = producer.create_frame(frame::Info {
			size: MAX_GROUP_CACHE + 1,
			timestamp: Timestamp::ZERO,
		});
		assert!(matches!(result, Err(Error::FrameTooLarge)));
	}
}
