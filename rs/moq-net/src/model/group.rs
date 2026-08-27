//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once ([Producer::write_frame]), or in chunks
//! ([Producer::create_frame]).
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [Error] when all writers or readers are dropped.
use crate::cache;
use crate::frame::{self, Frame, FrameBuf};
use crate::{Timescale, stats, track};
use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, ready};

use crate::{Error, IntoBytes, Result, Timestamp};

/// Maximum total size of frames cached in a group before old frames are evicted.
///
/// Doubles as the per-frame size cap: a single frame can be at most this large (a
/// larger declared size is refused before allocating), so one maximum-size frame can
/// fill a group's cache.
pub(super) const MAX_GROUP_CACHE: u64 = 32 * 1024 * 1024; // 32 MB

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

	// The number of frames evicted from the front of the group.
	pub(crate) offset: usize,

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
			// A frame read is a cache access: stamp it so expiry and the eviction
			// walk spare a group a consumer is actively draining.
			self.charge.refresh();
			let info = frame::Info {
				size: f.payload.len() as u64,
				timestamp: f.timestamp,
			};
			return Poll::Ready(Ok(Some((info, frame::Source::Complete(f.payload.clone())))));
		}
		if local == self.frames.len()
			&& let Some(p) = &self.partial
		{
			self.charge.refresh();
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
		//
		// Check Ok and Err: Ok is unreachable after a deliberate close.
		match self.state.write() {
			Ok(mut state) => {
				if state.fin.is_some() || state.abort.is_some() {
					return;
				}
				tracing::warn!(
					sequence = self.info.sequence,
					"group::Producer dropped without finish() or abort()"
				);
				state.release();
			}
			Err(state) => {
				if state.fin.is_some() || state.abort.is_some() {
					return;
				}
				tracing::warn!(
					sequence = self.info.sequence,
					"group::Producer dropped without finish() or abort()"
				);
			}
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

		let mut state = self.writable()?;
		let size = payload.len() as u64;
		state.cache += size;
		state.charge.add(size);
		state.frames.push_back(Frame { timestamp, payload });
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

	/// Take the group state for a write, refusing one that can no longer accept frames.
	///
	/// A group with an open frame rejects rather than appends: `create_frame` borrows
	/// its producer exclusively, but `Producer` is `Clone`, so a second handle can
	/// reach this while the first is still streaming. Appending around the open frame
	/// would hand readers the batch before the frame that was opened first.
	fn writable(&self) -> Result<kio::Mut<'_, GroupState>> {
		let state = modify(&self.state)?;
		if state.fin.is_some() {
			return Err(Error::Closed);
		}
		if state.partial.is_some() {
			return Err(Error::FrameOpen);
		}
		Ok(state)
	}

	/// Write a whole batch of frames at once, draining `frames`.
	///
	/// One lock covers the batch, so an ingest with several frames in hand pays the
	/// group mutex and the track's eviction settle once rather than per frame. Build
	/// the batch with [`frame::Buffer::push`].
	///
	/// The batch is validated before anything is written, so a rejected frame leaves
	/// both the group and the buffer exactly as they were, ready to retry or redirect.
	/// Returns [`Error::FrameOpen`] if another handle is streaming a frame into this
	/// group, since appending around it would reorder the group.
	pub fn write_frames<const N: usize>(&mut self, frames: &mut frame::Buffer<N>) -> Result<()> {
		// Check the whole batch up front, without touching it: a rejected batch stays
		// exactly as the caller built it, so it can be retried or sent elsewhere.
		// Timestamp conversion is lossy across scales that don't divide evenly, so
		// converting in place here would silently shift presentation times on retry.
		for frame in frames.filled() {
			frame
				.timestamp
				.convert(self.track.timescale)
				.map_err(|_| Error::TimestampMismatch)?;
			if frame.payload.len() as u64 > MAX_GROUP_CACHE {
				return Err(Error::FrameTooLarge);
			}
		}

		let count = frames.len() as u64;
		let mut bytes = 0;

		let mut state = self.writable()?;
		// Past every fallible check: converting again can't fail, and the batch is
		// ours from here.
		for mut frame in frames.drain() {
			frame.timestamp = frame
				.timestamp
				.convert(self.track.timescale)
				.expect("timestamp scale checked above");
			let size = frame.payload.len() as u64;
			bytes += size;
			state.cache += size;
			state.charge.add(size);
			state.frames.push_back(frame);
		}
		state.evict();
		drop(state);

		// With the group lock released (lock order is track then group), settle
		// eviction debt if enough has been written since the track last paid.
		self.cache.settle();

		// Ingress payload: the whole batch, counted once.
		self.stats.frames(count);
		self.stats.bytes(bytes);
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

		let mut state = self.writable()?;
		state.cache += frame.size;
		state.charge.add(frame.size);
		state.partial = Some(Partial {
			timestamp,
			buf: buf.clone(),
		});
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
		// The chunk that was just written is a write access: restart the retention
		// clock so a straggler group streaming a large frame isn't expired
		// mid-write (its bytes were already charged when the frame was created).
		// `record_write` takes `&mut`, which marks the guard modified: kio only
		// notifies on a mutably-accessed guard's release, and that notify is what
		// delivers the chunk to parked readers.
		if let Ok(mut state) = self.state.write() {
			state.charge.record_write();
		}
	}

	/// Commit the in-flight frame as a completed frame (called by [`frame::Producer::finish`]).
	pub(crate) fn frame_commit(&mut self, frame: Frame) -> Result<()> {
		let mut state = modify(&self.state)?;
		// Bytes were already counted against the cache (and the pool charge) when the
		// frame was created; committing just moves the tail into the completed set.
		state.partial = None;
		state.frames.push_back(frame);
		Ok(())
	}

	/// Fail the group because an in-flight frame couldn't complete (called by
	/// [`frame::Producer::abort`] / its drop).
	pub(crate) fn frame_abort(&mut self, err: Error) {
		let _ = self.clone().abort(err);
	}

	/// Return the number of frames written so far (completed plus any in-flight).
	pub fn frame_count(&self) -> usize {
		let state = self.state.read();
		state.offset + state.frames.len() + state.partial.is_some() as usize
	}

	/// Mark the group as complete; no more frames will be written.
	///
	/// Borrows rather than consumes, so a later failure can still be reported through
	/// [`abort`](Self::abort). The handle also keeps the cached frames readable.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = modify(&self.state)?;
		// The recorded count is what tells readers the group ended, so an open frame
		// would be left out of it and read as a clean end rather than a frame still
		// coming. Another clone can reach this while the frame's producer holds the
		// handle, so refuse rather than strand it. Use `abort` to end a group early.
		if state.partial.is_some() {
			return Err(Error::FrameOpen);
		}
		state.fin = Some(state.offset + state.frames.len());
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

	/// Record a cache access (delivery to a subscriber, a FETCH hit, or a fetched
	/// backfill's birth), protecting the group from eviction and restarting its
	/// expiry clock. Stamps through a read guard, whose release never notifies, so
	/// delivery can't wake every consumer parked on the group. Harmless on a
	/// closed group: its charge is already cleared.
	pub(crate) fn cache_refresh(&self) {
		self.state.read().charge.refresh();
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info,
			state: self.state.consume(),
			track: self.track.clone(),
			index: 0,
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

/// Consume a group, frame-by-frame.
pub struct Consumer {
	// Shared state with the producer.
	state: kio::Consumer<GroupState>,

	// Immutable stream state.
	info: Info,

	// The parent track's info, inherited from the producer. Its `timescale` lets the
	// wire publisher emit per-frame timestamps at the right scale for a fetched group.
	track: track::Info,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,

	// Egress payload meter, set by a tagged track via [`Self::with_meter`]. Empty
	// (no-op) for an untagged group.
	stats: stats::Meter,
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		// A clone shares the channel and inherits `index`, but then runs in parallel.
		Self {
			state: self.state.clone(),
			info: self.info,
			track: self.track.clone(),
			index: self.index,
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
	/// Attach an egress payload meter, counting this as one delivered group.
	/// Called by a tagged track when it hands the consumer to a subscriber or fetch.
	pub(crate) fn with_meter(mut self, meter: stats::Meter) -> Self {
		meter.group();
		self.stats = meter;
		self
	}

	/// Whether the group has been aborted (including pool eviction); the abort
	/// dropped the cached frames, so a held consumer has nothing left to read.
	pub(crate) fn is_aborted(&self) -> bool {
		self.state.read().abort.is_some()
	}

	/// Mark the group as still being read, so a slow drain doesn't expire it.
	///
	/// [`Self::read_frames`] stamps the group's cache access once per batch, which
	/// bounds frames rather than elapsed time. A reader that takes longer than the
	/// track's `latency_max` to work through one batch (a publisher writing to a
	/// flow-controlled peer, say) calls this between frames, or the rest of the group
	/// is expired out from under it mid-serve. [`Self::read_frame`] stamps on every
	/// call and needs no help.
	///
	/// Cheap and idempotent within a coarse clock tick, so calling it per frame is
	/// fine.
	pub fn keep_alive(&self) {
		self.state.read().charge.refresh();
	}

	/// Record a cache access from the consumer side: a parked group re-offered to
	/// its subscriber. Same stamp as [`Producer::cache_refresh`].
	pub(crate) fn cache_refresh(&self) {
		self.keep_alive();
	}

	/// Park `waiter` until the group closes (finish, abort, or eviction). Spliced
	/// subscribers register on parked groups so an eviction wakes them; a group
	/// that already closed cleanly can never abort, so no waiter is needed.
	pub(crate) fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_closed(waiter)
	}

	/// The parent track's timescale.
	pub fn timescale(&self) -> Timescale {
		self.track.timescale
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: FnMut(&kio::Ref<'_, GroupState>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Return a consumer for the next frame for chunked reading.
	pub async fn next_frame(&mut self) -> Result<Option<frame::Consumer>> {
		kio::wait(|waiter| self.poll_next_frame(waiter)).await
	}

	/// Poll for the next frame, without blocking.
	///
	/// Returns None if the group is finished and the index is out of range.
	pub fn poll_next_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Consumer>>> {
		let index = self.index;
		let Some((info, source)) = ready!(self.poll(waiter, |state| state.poll_frame_source(index))?) else {
			return Poll::Ready(Ok(None));
		};

		self.index += 1;
		// Count the frame here; the frame::Consumer counts its bytes per chunk as
		// they're read out.
		self.stats.frames(1);
		Poll::Ready(Ok(Some(
			frame::Consumer::new(self.state.clone(), info, source).with_meter(self.stats.clone()),
		)))
	}

	/// Read the next frame (timestamp and payload) all at once, without blocking.
	///
	/// Use [`Self::read_frames`] to pull a whole batch under one lock; a group of small
	/// frames drains several times faster that way.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		let index = self.index;
		let frame = ready!(self.poll(waiter, |state| {
			if index < state.offset {
				return Poll::Ready(Err(Error::Lagged));
			}
			if let Some(frame) = state.frames.get(index - state.offset) {
				// A frame read is a cache access: stamp it so expiry and the eviction
				// walk spare a group a consumer is actively draining.
				state.charge.refresh();
				return Poll::Ready(Ok(Some(frame.clone())));
			}
			// Nothing completed at `index`: an in-flight tail waits, otherwise resolve
			// the terminal state (whole-frame reads never stream the partial).
			state.poll_terminal(index).map_ok(|()| None)
		})?);

		if let Some(frame) = &frame {
			self.index += 1;
			self.stats.frames(1);
			self.stats.bytes(frame.payload.len() as u64);
		}

		Poll::Ready(Ok(frame))
	}

	/// Read the next frame (timestamp and payload) all at once.
	pub async fn read_frame(&mut self) -> Result<Option<frame::Frame>> {
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Fill `out` with every frame that is ready, up to its capacity, without blocking.
	///
	/// Returns how many frames were written; they're in [`frame::Buffer::filled`]. The
	/// buffer's previous batch is dropped first, so one buffer serves a whole group.
	///
	/// This is a *short* read: it returns as soon as anything is ready rather than
	/// waiting for `out` to fill, so a partial batch does not mean the group ended.
	/// Only a count of `0` does (and only for a non-zero capacity).
	///
	/// One stamp covers the whole batch, so a slow drain calls
	/// [`Self::keep_alive`] between frames.
	pub fn poll_read_frames<const N: usize>(
		&mut self,
		waiter: &kio::Waiter,
		out: &mut frame::Buffer<N>,
	) -> Poll<Result<usize>> {
		// Drop the previous batch before taking the lock: deallocating payloads is the
		// caller's cost to pay, not something to hold the group's mutex through.
		out.clear();

		let index = self.index;
		let res = self.poll(waiter, |state| {
			if index < state.offset {
				return Poll::Ready(Err(Error::Lagged));
			}
			// `local` can run past the buffered count when frames were cleared or evicted
			// out from under us (abort, unfinished drop, an eviction gap); clamp so
			// `range` never panics on an out-of-bounds start.
			let local = (index - state.offset).min(state.frames.len());
			if out.fill(state.frames.range(local..).cloned()) > 0 {
				// One stamp covers the whole batch.
				state.charge.refresh();
				return Poll::Ready(Ok(()));
			}
			// An empty fill means nothing completed at `index`: park on an in-flight
			// tail, otherwise resolve the terminal state. A finished group resolves to
			// `Ok`, leaving the zero count to report the end.
			state.poll_terminal(index)
		});

		// A `Pending` here leaves `out` cleared, which is what an empty batch should look
		// like to a caller that inspects it anyway.
		ready!(res)?;

		let filled = out.filled().len();
		self.index += filled;
		// Count the whole batch once, under no lock.
		self.stats.frames(filled as u64);
		self.stats
			.bytes(out.filled().iter().map(|f| f.payload.len() as u64).sum());

		Poll::Ready(Ok(filled))
	}

	/// Fill `out` with every frame that is ready, blocking until at least one is or the
	/// group ends. Returns the batch, empty only at the end of the group.
	///
	/// See [`Self::poll_read_frames`] for the short-read semantics.
	pub async fn read_frames<'a, const N: usize>(
		&mut self,
		out: &'a mut frame::Buffer<N>,
	) -> Result<&'a mut [frame::Frame]> {
		// The closure reborrows `out` for less than `'a`, so the buffer is free again
		// once the wait resolves.
		kio::wait(|waiter| self.poll_read_frames(waiter, out)).await?;
		Ok(out.filled_mut())
	}

	/// Poll for the final number of frames in the group.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		self.poll(waiter, |state| state.poll_finished())
	}

	/// Block until the group is finished, returning the number of frames in the group.
	pub async fn finished(&mut self) -> Result<u64> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}
}

/// Options for a one-shot [`track::Consumer::fetch_group`] of a past group.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
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

#[cfg(test)]
mod test {
	use super::*;
	use crate::model::test_tracing::count_drop_warnings;
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

	/// Write `n` frames with payloads "0".."n-1" into a fresh group.
	fn filled_group(n: usize) -> Producer {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..n {
			producer
				.write_frame(Timestamp::ZERO, Bytes::from(i.to_string()))
				.unwrap();
		}
		producer
	}

	/// The payload strings of a batch.
	fn payloads(frames: &[Frame]) -> Vec<String> {
		frames
			.iter()
			.map(|frame| String::from_utf8(frame.payload.to_vec()).unwrap())
			.collect()
	}

	/// Drain a consumer through a batch buffer of `N`, collecting payload strings.
	fn drain<const N: usize>(consumer: &mut Consumer) -> Vec<String> {
		let mut buf = frame::Buffer::<N>::new();
		let mut seen = Vec::new();
		loop {
			let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
			if batch.is_empty() {
				break;
			}
			seen.extend(payloads(batch));
		}
		seen
	}

	/// `create_frame` borrows its producer exclusively, but `Producer` is `Clone`, so a
	/// second handle can reach the whole-frame writes while a frame is still open.
	/// Appending there would hand readers the new frames before the one opened first,
	/// so every whole-frame path refuses instead.
	#[test]
	fn writes_are_refused_while_a_frame_is_open() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut other = producer.clone();

		// One handle opens a frame and holds it, incomplete.
		let mut open = producer
			.create_frame(frame::Info {
				size: 4,
				timestamp: Timestamp::ZERO,
			})
			.unwrap();

		let mut buf = frame::Buffer::<4>::new();
		buf.push(frame::Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(b"batch"),
		})
		.unwrap();

		assert!(matches!(other.write_frames(&mut buf), Err(Error::FrameOpen)));
		assert_eq!(buf.len(), 1, "the batch is still the caller's");
		assert!(matches!(
			other.write_frame(Timestamp::ZERO, Bytes::from_static(b"single")),
			Err(Error::FrameOpen)
		));
		assert!(matches!(
			other
				.create_frame(frame::Info {
					size: 1,
					timestamp: Timestamp::ZERO,
				})
				.err(),
			Some(Error::FrameOpen)
		));

		// Once the open frame lands, the group takes writes again in order.
		open.write(&b"open"[..]).unwrap();
		open.finish().unwrap();
		other.write_frames(&mut buf).unwrap();
		other.finish().unwrap();

		let mut consumer = other.consume();
		assert_eq!(drain::<4>(&mut consumer), ["open", "batch"]);
	}

	/// Finishing records the frame count, and a batch read consults that count to
	/// decide the group ended. `create_frame` borrows its producer exclusively, but
	/// `Producer` is `Clone`, so a second handle can finish the group while the first
	/// is still writing a frame. The open frame would be left out of the count and
	/// read as a clean end of group, so a publisher would close the stream without
	/// ever sending it.
	#[test]
	fn finish_is_refused_while_a_frame_is_open() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut other = producer.clone();
		let mut consumer = producer.consume();

		let mut frame = producer
			.create_frame(frame::Info {
				size: 4,
				timestamp: Timestamp::ZERO,
			})
			.unwrap();

		assert!(matches!(other.finish(), Err(Error::FrameOpen)));

		// Not "the group ended": the batch read parks until the frame lands.
		let mut buf = frame::Buffer::<4>::new();
		assert!(
			consumer.read_frames(&mut buf).now_or_never().is_none(),
			"an open frame must not read as the end of the group"
		);

		frame.write(&b"open"[..]).unwrap();
		frame.finish().unwrap();
		other.finish().unwrap();

		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert_eq!(batch.len(), 1);
		assert_eq!(batch[0].payload, Bytes::from_static(b"open"));
	}

	#[test]
	fn write_frames_appends_the_whole_batch() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut buf = frame::Buffer::<8>::new();
		for i in 0..5u8 {
			buf.push(frame::Frame {
				timestamp: Timestamp::ZERO,
				payload: Bytes::from(i.to_string()),
			})
			.unwrap();
		}
		producer.write_frames(&mut buf).unwrap();
		assert!(buf.is_empty(), "the batch was drained");
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		assert_eq!(drain::<8>(&mut consumer), ["0", "1", "2", "3", "4"]);
	}

	/// A rejected frame must leave both the group and the batch untouched, or the
	/// caller has no way to tell what was written.
	#[test]
	fn write_frames_rejects_the_batch_atomically() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut buf = frame::Buffer::<4>::new();
		buf.push(frame::Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(b"ok"),
		})
		.unwrap();
		// Larger than the group's whole byte budget.
		buf.push(frame::Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from(vec![0u8; MAX_GROUP_CACHE as usize + 1]),
		})
		.unwrap();

		assert!(matches!(producer.write_frames(&mut buf), Err(Error::FrameTooLarge)));
		assert_eq!(buf.len(), 2, "the batch is still the caller's");

		producer.finish().unwrap();
		let mut consumer = producer.consume();
		assert!(drain::<4>(&mut consumer).is_empty(), "nothing was written");
	}

	/// A batch rejected mid-validation must leave the caller's frames byte-identical,
	/// including their timestamps: converting in place would compound scale loss if
	/// the batch is retried against another track.
	#[test]
	fn write_frames_leaves_a_rejected_batch_unconverted() {
		use crate::Timescale;

		let mut producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
			Default::default(),
		);

		let mut buf = frame::Buffer::<4>::new();
		buf.push(frame::Frame {
			timestamp: Timestamp::from_millis(1).unwrap(),
			payload: Bytes::from_static(b"ok"),
		})
		.unwrap();
		// Refused after the first frame would already have been converted in place.
		buf.push(frame::Frame {
			timestamp: Timestamp::from_millis(2).unwrap(),
			payload: Bytes::from(vec![0u8; MAX_GROUP_CACHE as usize + 1]),
		})
		.unwrap();

		assert!(matches!(producer.write_frames(&mut buf), Err(Error::FrameTooLarge)));
		let kept = buf.filled();
		assert_eq!(kept.len(), 2, "the batch is still the caller's");
		assert_eq!(kept[0].timestamp.scale(), Timescale::MILLI, "timestamp was rewritten");
		assert_eq!(kept[0].timestamp.value(), 1);
	}

	/// A batch that is accepted still converts into the track's scale.
	#[test]
	fn write_frames_converts_into_the_track_scale() {
		use crate::Timescale;

		let mut producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
			Default::default(),
		);

		let mut buf = frame::Buffer::<4>::new();
		buf.push(frame::Frame {
			timestamp: Timestamp::from_millis(1).unwrap(),
			payload: Bytes::from_static(b"x"),
		})
		.unwrap();
		producer.write_frames(&mut buf).unwrap();
		producer.finish().unwrap();

		let frame = producer
			.consume()
			.read_frame()
			.now_or_never()
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp.scale(), Timescale::MICRO);
		assert_eq!(frame.timestamp.value(), 1000);
	}

	#[test]
	fn buffer_push_refuses_past_capacity() {
		let mut buf = frame::Buffer::<2>::new();
		let frame = || frame::Frame {
			timestamp: Timestamp::ZERO,
			payload: Bytes::from_static(b"x"),
		};
		buf.push(frame()).unwrap();
		buf.push(frame()).unwrap();
		assert!(buf.is_full());
		assert!(buf.push(frame()).is_err(), "a full buffer hands the frame back");
	}

	/// A partially consumed drain still empties the buffer, dropping the rest.
	#[test]
	fn buffer_drain_empties_even_when_abandoned() {
		let mut buf = frame::Buffer::<4>::new();
		for i in 0..4u8 {
			buf.push(frame::Frame {
				timestamp: Timestamp::ZERO,
				payload: Bytes::from(vec![i; 1]),
			})
			.unwrap();
		}
		let taken: Vec<_> = buf.drain().take(2).collect();
		assert_eq!(taken.len(), 2);
		assert!(buf.is_empty(), "an abandoned drain still empties the buffer");
	}

	#[test]
	fn read_frames_fills_whole_batch() {
		let mut producer = filled_group(5);
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let mut buf = frame::Buffer::<8>::new();

		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert_eq!(payloads(batch), ["0", "1", "2", "3", "4"]);

		// A finished group reports the end with an empty batch.
		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert!(batch.is_empty());
	}

	#[test]
	fn read_frames_bounded_by_capacity() {
		let mut producer = filled_group(5);
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		assert_eq!(drain::<2>(&mut consumer), ["0", "1", "2", "3", "4"]);
	}

	#[test]
	fn read_frames_resumes_after_a_single_read() {
		let mut producer = filled_group(12);
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let first = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(first.payload, Bytes::from_static(b"0"));

		assert_eq!(
			drain::<8>(&mut consumer),
			["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"]
		);
	}

	#[test]
	fn read_frames_returns_short_instead_of_waiting() {
		let mut producer = filled_group(2);

		let mut consumer = producer.consume();
		let mut buf = frame::Buffer::<8>::new();

		// The group is still open, so the batch is short rather than blocking for more.
		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert_eq!(payloads(batch), ["0", "1"]);

		// Nothing left and no terminal state: this one parks.
		assert!(consumer.read_frames(&mut buf).now_or_never().is_none());

		producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"2")).unwrap();
		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert_eq!(payloads(batch), ["2"]);
	}

	#[test]
	fn read_frames_reports_an_abort() {
		let producer = filled_group(2);
		let mut consumer = producer.consume();
		producer.abort(Error::Cancel).unwrap();

		// The abort released the cached frames, so nothing survives it.
		let mut buf = frame::Buffer::<8>::new();
		let res = consumer.read_frames(&mut buf).now_or_never().unwrap();
		assert!(matches!(res, Err(Error::Cancel)));
	}

	/// A refill drops the previous batch, so a reused buffer never accumulates frames.
	#[test]
	fn read_frames_refill_replaces_the_previous_batch() {
		let mut producer = filled_group(3);
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let mut buf = frame::Buffer::<2>::new();

		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert_eq!(payloads(batch), ["0", "1"]);

		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert_eq!(payloads(batch), ["2"]);
		assert_eq!(buf.filled().len(), 1, "the buffer holds only the latest batch");
	}

	#[test]
	fn read_frames_zero_capacity_reads_nothing() {
		let mut producer = filled_group(2);
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let mut buf = frame::Buffer::<0>::new();
		let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
		assert!(batch.is_empty());

		// The reader did not advance.
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, Bytes::from_static(b"0"));
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
	fn drop_after_abort_does_not_warn() {
		let warns = count_drop_warnings("group::Producer dropped without finish", || {
			let producer = Info { sequence: 0 }.produce();
			let keep = producer.clone();
			let mut writer = producer.clone();
			writer
				.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
				.unwrap();
			let _consumer = producer.consume();
			writer.abort(crate::Error::Cancel).unwrap();
			drop(keep);
		});
		assert_eq!(warns, 0, "abort-then-drop must not emit unfinished-producer WARN");
	}

	#[test]
	fn drop_unfinished_warns() {
		let warns = count_drop_warnings("group::Producer dropped without finish", || {
			let producer = Info { sequence: 0 }.produce();
			let mut writer = producer.clone();
			writer
				.write_frame(Timestamp::ZERO, Bytes::from_static(b"data"))
				.unwrap();
			let _consumer = producer.consume();
			drop(writer);
			drop(producer);
		});
		assert!(warns >= 1, "unfinished drop must emit unfinished-producer WARN");
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

	/// Refilling a buffer several times drains every frame in order across the batch
	/// boundary (each refill starts exactly where the previous batch ended).
	#[test]
	fn read_frames_crosses_batches() {
		const CAP: usize = 8;
		let n = CAP * 3 + 5;
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..n {
			producer
				.write_frame(Timestamp::ZERO, Bytes::from(vec![i as u8; 4]))
				.unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let mut buf = frame::Buffer::<CAP>::new();
		let mut seen = 0;
		loop {
			let batch = consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap();
			if batch.is_empty() {
				break;
			}
			for frame in batch.iter() {
				assert_eq!(frame.payload, Bytes::from(vec![seen as u8; 4]));
				seen += 1;
			}
		}
		assert_eq!(seen, n);
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

	/// `next_frame` picks up where a prior `read_frame` left off, preserving order.
	#[test]
	fn interleave_read_and_next_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..5u8 {
			producer.write_frame(Timestamp::ZERO, Bytes::from(vec![i; 1])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let f0 = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.payload, Bytes::from(vec![0u8; 1]));

		// next_frame must continue from there, not skip ahead or repeat.
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

	/// Dropping a filled buffer must drop its frames rather than leak them
	/// (exercises the `MaybeUninit` Drop path; run under miri to catch leaks/UB).
	#[test]
	fn drop_with_a_filled_buffer() {
		const CAP: usize = 8;
		let mut producer = Info { sequence: 0 }.produce();
		for _ in 0..CAP {
			producer.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let mut buf = frame::Buffer::<CAP>::new();
		// Fill the buffer, then drop it without taking anything out.
		assert_eq!(
			consumer.read_frames(&mut buf).now_or_never().unwrap().unwrap().len(),
			CAP
		);
		drop(buf);
	}

	/// A parked chunk reader is woken by each chunk write. kio only notifies when
	/// a write guard was mutably accessed, so `frame_notify` must mark the guard
	/// modified; a guard dropped untouched wakes nobody and the reader would
	/// stall until the frame completed.
	#[tokio::test]
	async fn chunk_write_wakes_parked_reader() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		let mut frame = producer
			.create_frame(frame::Info {
				size: 6,
				timestamp: Timestamp::ZERO,
			})
			.unwrap();
		let mut f = consumer.next_frame().await.unwrap().unwrap();
		let handle = tokio::spawn(async move { f.read_chunk().await });
		// Let the reader park on the empty partial before the chunk lands.
		tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		frame.write(Bytes::from_static(b"foo")).unwrap();
		let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
			.await
			.expect("parked chunk reader was never woken by the chunk write")
			.unwrap()
			.unwrap();
		assert_eq!(chunk, Some(Bytes::from_static(b"foo")));
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
