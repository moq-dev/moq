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
use crate::{Timescale, track};
use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::task::{Poll, ready};

use bytes::Bytes;

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
		Producer::new(self, track::Info::default())
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

	// This group's registration in the track's cache pool; mirrors `cache` so the
	// pool can evict the least-recently-read groups under memory pressure.
	charge: cache::Charge,

	// Whether the group has been finalized (no more frames).
	pub(crate) fin: bool,

	// The error that caused the group to be aborted, if any.
	pub(crate) abort: Option<Error>,
}

impl GroupState {
	/// Resolve the source for the frame at `index`: a completed frame (whole) or the
	/// in-flight tail (streamed). Used by [`Consumer::poll_next_frame`].
	fn poll_frame_source(&self, index: usize) -> Poll<Result<Option<(frame::Info, frame::Source)>>> {
		if index < self.offset {
			return Poll::Ready(Err(Error::CacheFull));
		}
		let local = index - self.offset;
		if let Some(f) = self.frames.get(local) {
			self.charge.touch();
			let info = frame::Info {
				size: f.payload.len() as u64,
				timestamp: f.timestamp,
			};
			return Poll::Ready(Ok(Some((info, frame::Source::Complete(f.payload.clone())))));
		}
		if local == self.frames.len()
			&& let Some(p) = &self.partial
		{
			self.charge.touch();
			let info = frame::Info {
				size: p.buf.capacity() as u64,
				timestamp: p.timestamp,
			};
			return Poll::Ready(Ok(Some((info, frame::Source::Partial(p.buf.clone())))));
		}
		// `abort` is checked before `fin`: an evicted group is both finished and
		// aborted with its frames cleared, and the reader must see the abort rather
		// than a clean end-of-group at the wrong index.
		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if self.fin {
			return Poll::Ready(Ok(None));
		}
		Poll::Pending
	}

	fn poll_finished(&self) -> Poll<Result<u64>> {
		if let Some(err) = &self.abort {
			// Checked before `fin`: an evicted group is both finished and aborted,
			// and its cleared frames would report a bogus count.
			Poll::Ready(Err(err.clone()))
		} else if self.fin {
			Poll::Ready(Ok((self.offset + self.frames.len()) as u64))
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

/// The pool's eviction hook: abort the group with [`Error::Evicted`], freeing its
/// frames immediately. A no-op once the group is already aborted or fully dropped.
fn evict(state: &kio::Weak<GroupState>) {
	let Ok(mut state) = state.write() else { return };
	if state.abort.is_some() {
		return;
	}
	state.abort = Some(Error::Evicted);
	state.release();
	state.close();
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

	// The parent track's info, inherited rather than passed piecemeal. Its
	// `timescale` is used by [`Self::create_frame`] to normalize every frame's
	// timestamp into the track scale before it enters the stream. Threaded down by
	// value from [`track::Producer::create_group`] / `append_group`.
	track: track::Info,
}

impl std::ops::Deref for Producer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Producer {
	/// Create a group producer bound to its parent track's [`track::Info`].
	///
	/// Crate-private: groups are only constructed via [`track::Producer`], which
	/// threads its [`track::Info`] down so properties like the timescale are inherited
	/// rather than passed in. Every frame added to this group is normalized to the
	/// track's timescale by [`Self::create_frame`].
	///
	/// Registers the group in the shared cache pool reached through the track's
	/// broadcast (`track.broadcast.origin.pool`), so its cached bytes count against
	/// the budget and it can be evicted under memory pressure.
	pub(crate) fn new(info: Info, track: track::Info) -> Self {
		let state = kio::Producer::<GroupState>::default();
		let weak = state.weak();
		let charge = track.broadcast.origin.pool.register(Box::new(move || evict(&weak)));
		state.write().ok().expect("a new group is open").charge = charge;
		Self { info, state, track }
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
	/// `timestamp` is converted into the parent track's timescale. Use
	/// [Self::write_frame_now] to stamp wall-clock time instead of supplying one.
	pub fn write_frame<B: IntoBytes>(&mut self, timestamp: Timestamp, data: B) -> Result<()> {
		let timestamp = timestamp
			.convert(self.track.timescale)
			.map_err(|_| Error::TimestampMismatch)?;
		let payload = data.into_bytes();
		if payload.len() as u64 > MAX_GROUP_CACHE {
			return Err(Error::FrameTooLarge);
		}

		let mut state = modify(&self.state)?;
		if state.fin {
			return Err(Error::Closed);
		}
		debug_assert!(state.partial.is_none(), "a frame is already open");
		let size = payload.len() as u64;
		state.cache += size;
		state.charge.add(size);
		state.frames.push_back(Frame { timestamp, payload });
		state.evict();

		// The pool evicts other groups' state, so trigger it only after releasing our
		// lock. Reached via the parent chain; a no-op when the pool is unbounded.
		drop(state);
		self.track.broadcast.origin.pool.evict();
		Ok(())
	}

	/// Like [Self::write_frame] but stamps the frame with wall-clock now
	/// ([`Timestamp::now`]). For data with no real presentation time of its own
	/// (catalogs, JSON state) or sources whose protocol can't carry one.
	pub fn write_frame_now<B: IntoBytes>(&mut self, data: B) -> Result<()> {
		self.write_frame(Timestamp::now(), data)
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
		if state.fin {
			return Err(Error::Closed);
		}
		debug_assert!(state.partial.is_none(), "a frame is already open");
		state.cache += frame.size;
		state.charge.add(frame.size);
		state.partial = Some(Partial {
			timestamp,
			buf: buf.clone(),
		});
		state.evict();

		// The pool evicts other groups' state, so trigger it only after releasing our
		// lock. Reached via the parent chain; a no-op when the pool is unbounded.
		drop(state);
		self.track.broadcast.origin.pool.evict();

		let info = frame::Info {
			size: frame.size,
			timestamp,
		};
		Ok(frame::Producer::new(self, buf, info))
	}

	/// Like [Self::create_frame] but stamps the frame with wall-clock now
	/// ([`Timestamp::now`]).
	pub fn create_frame_now(&mut self, size: u64) -> Result<frame::Producer<'_>> {
		self.create_frame(frame::Info {
			size,
			timestamp: Timestamp::now(),
		})
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
		Ok(())
	}

	/// Fail the group because an in-flight frame couldn't complete (called by
	/// [`frame::Producer::abort`] / its drop).
	pub(crate) fn frame_abort(&mut self, err: Error) {
		let _ = self.abort(err);
	}

	/// Return the number of frames written so far (completed plus any in-flight).
	pub fn frame_count(&self) -> usize {
		let state = self.state.read();
		state.offset + state.frames.len() + state.partial.is_some() as usize
	}

	/// Mark the group as complete; no more frames will be written.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = modify(&self.state)?;
		state.fin = true;
		Ok(())
	}

	/// Abort the group with the given error.
	///
	/// No updates can be made after this point. Drops the cached frames so a stale
	/// [`Consumer`] can't pin their buffers in memory forever; consumers that haven't
	/// drained yet surface the abort error instead of the leftover cache.
	pub fn abort(&mut self, err: Error) -> Result<()> {
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

	/// This group's cache pool registration, used by the track to pin the latest
	/// group. `None` when the pool is detached (the unbounded default).
	pub(crate) fn cache_entry(&self) -> Option<Arc<cache::Entry>> {
		self.state.read().charge.entry()
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> Consumer {
		Consumer {
			info: self.info,
			state: self.state.consume(),
			track: self.track.clone(),
			index: 0,
			prefetch: Prefetch::default(),
		}
	}

	/// Block until the group is closed or aborted.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}
}

impl Clone for Producer {
	fn clone(&self) -> Self {
		Self {
			info: self.info,
			state: self.state.clone(),
			track: self.track.clone(),
		}
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		// See track::Producer::drop: the last producer dropping without a clean finish
		// releases the cached frames so a stale consumer can't pin their buffers forever.
		// A finished group keeps its cache so consumers can drain.
		if !self.state.is_last() {
			return;
		}
		if let Ok(mut state) = modify(&self.state)
			&& !state.fin
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

	// A batch of completed frames drained ahead under one lock (whole-frame reads only).
	prefetch: Prefetch,
}

impl Clone for Consumer {
	fn clone(&self) -> Self {
		// A clone shares the channel and inherits `index`, but starts with an empty
		// prefetch: it re-reads its batch from the shared state, in parallel.
		Self {
			state: self.state.clone(),
			info: self.info,
			track: self.track.clone(),
			index: self.index,
			prefetch: Prefetch::default(),
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
	/// The parent track's timescale.
	pub fn timescale(&self) -> Timescale {
		self.track.timescale
	}

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

	/// Return a consumer for the next frame for chunked reading.
	pub async fn next_frame(&mut self) -> Result<Option<frame::Consumer>> {
		kio::wait(|waiter| self.poll_next_frame(waiter)).await
	}

	/// Poll for the next frame, without blocking.
	///
	/// Returns None if the group is finished and the index is out of range.
	pub fn poll_next_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Consumer>>> {
		// Hand out any frames a prior read_frame prefetched before touching the tail.
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
		Poll::Ready(Ok(Some(frame::Consumer::new(self.state.clone(), info, source))))
	}

	/// Read the next frame's data all at once, without blocking.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		// Fast path: serve from the prefetched batch without locking or allocating a waker.
		if let Some(frame) = self.prefetch.pop() {
			self.index += 1;
			return Poll::Ready(Ok(Some(frame.payload)));
		}

		// The batch is drained: refill it under a single lock, registering the waiter if
		// nothing is ready. Borrow the two fields disjointly so the closure can fill.
		let index = self.index;
		let prefetch = &mut self.prefetch;
		let res = self.state.poll(waiter, |state| {
			if index < state.offset {
				return Poll::Ready(Err(Error::CacheFull));
			}
			// `local` can run past the buffered count when frames were cleared or evicted out
			// from under us (abort, unfinished drop, an eviction gap); clamp so `range` never
			// panics on an out-of-bounds start. `fill` always resets the batch, so an empty
			// range leaves `len == 0` and the terminal checks below resolve abort/fin/pending.
			let local = (index - state.offset).min(state.frames.len());
			prefetch.fill(state.frames.range(local..).cloned());
			if prefetch.len > 0 {
				// Mark the group recently read so the cache pool keeps it over staler
				// groups. Touching once per batch fill is enough; the drained pops that
				// follow serve from the prefetch without re-locking.
				state.charge.touch();
				return Poll::Ready(Ok(()));
			}
			// Nothing completed at `index`: an in-flight tail waits, otherwise resolve
			// the terminal state (whole-frame reads never stream the partial).
			if let Some(err) = &state.abort {
				return Poll::Ready(Err(err.clone()));
			}
			if state.fin {
				return Poll::Ready(Ok(()));
			}
			Poll::Pending
		});

		match ready!(res) {
			Ok(Ok(())) => {}
			Ok(Err(err)) => return Poll::Ready(Err(err)),
			Err(state) => return Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
		}

		Poll::Ready(Ok(self.prefetch.pop().map(|frame| {
			self.index += 1;
			frame.payload
		})))
	}

	/// Read the next frame's data all at once.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>> {
		// Serve from the prefetched batch without building a future or allocating a waker.
		if let Some(frame) = self.prefetch.pop() {
			self.index += 1;
			return Ok(Some(frame.payload));
		}
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
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
	use futures::FutureExt;

	#[test]
	fn basic_frame_reading() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame_now(Bytes::from_static(b"frame0")).unwrap();
		producer.write_frame_now(Bytes::from_static(b"frame1")).unwrap();
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
		producer.write_frame_now(Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"hello"));
	}

	#[test]
	fn chunked_frame_reads_whole() {
		let mut producer = Info { sequence: 0 }.produce();
		{
			let mut frame = producer.create_frame_now(10).unwrap();
			frame.write(Bytes::from_static(b"hello")).unwrap();
			frame.write(Bytes::from_static(b"world")).unwrap();
			frame.finish().unwrap();
		}
		producer.finish().unwrap();

		// Frame data is held in a single per-frame buffer; a whole-frame read returns
		// the full contents in one slice.
		let mut consumer = producer.consume();
		let data = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"helloworld"));
	}

	#[test]
	fn chunked_frame_streams_partial() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		let mut frame = producer.create_frame_now(6).unwrap();
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
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		producer.abort(crate::Error::Cancel).unwrap();

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Cancel)));
	}

	#[test]
	fn abort_clears_cached_frames() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame_now(Bytes::from_static(b"data")).unwrap();

		// A stale consumer that never reads must not pin the cached frames.
		let _consumer = producer.consume();
		assert_eq!(producer.state.read().frames.len(), 1);

		producer.abort(crate::Error::Cancel).unwrap();

		let state = producer.state.read();
		assert!(state.frames.is_empty(), "cached frames should be dropped on abort");
		assert_eq!(state.cache, 0);
	}

	#[test]
	fn drop_unfinished_clears_cached_frames() {
		let producer = Info { sequence: 0 }.produce();
		let mut writer = producer.clone();
		writer.write_frame_now(Bytes::from_static(b"data")).unwrap();

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
		producer.write_frame_now(Bytes::from_static(b"data")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		drop(producer);

		// A cleanly finished group keeps its cache so the consumer can still drain.
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame, Bytes::from_static(b"data"));
	}

	#[tokio::test]
	async fn pending_then_ready() {
		let mut producer = Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		// Consumer blocks because no frames yet.
		assert!(consumer.next_frame().now_or_never().is_none());

		producer.write_frame_now(Bytes::from_static(b"data")).unwrap();
		producer.finish().unwrap();

		let frame = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.size, 4);
	}

	#[test]
	fn eviction_drops_old_frames() {
		let mut producer = Info { sequence: 0 }.produce();

		// Write frames that total more than MAX_GROUP_CACHE.
		let big = Bytes::from(vec![0u8; MAX_GROUP_CACHE as usize]);
		producer.write_frame_now(big.clone()).unwrap();
		producer.write_frame_now(big).unwrap();

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
		producer.write_frame_now(big.clone()).unwrap();
		producer.write_frame_now(big).unwrap();

		let mut consumer = producer.consume();
		// First frame was evicted, next_frame should return CacheFull.
		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::CacheFull)));
	}

	#[test]
	fn no_eviction_under_budget() {
		let mut producer = Info { sequence: 0 }.produce();
		// Many small frames stay cached: there is no frame-count cap, only a byte budget.
		for _ in 0..100_000 {
			producer.write_frame_now(Bytes::from_static(b"x")).unwrap();
		}
		producer.finish().unwrap();

		let state = producer.state.read();
		assert_eq!(state.offset, 0);
		assert_eq!(state.frames.len(), 100_000);
	}

	#[test]
	fn clone_consumer_independent() {
		let mut producer = Info { sequence: 0 }.produce();
		producer.write_frame_now(Bytes::from_static(b"a")).unwrap();

		let mut c1 = producer.consume();
		// Read one frame from c1
		let _ = c1.next_frame().now_or_never().unwrap().unwrap().unwrap();

		// Clone c1 — inherits index (past first frame)
		let mut c2 = c1.clone();

		producer.write_frame_now(Bytes::from_static(b"b")).unwrap();
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
			producer.write_frame_now(Bytes::from(vec![i as u8; 4])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		for i in 0..n {
			let data = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(data, Bytes::from(vec![i as u8; 4]));
		}
		assert!(consumer.read_frame().now_or_never().unwrap().unwrap().is_none());
	}

	/// `next_frame` drains frames a prior `read_frame` prefetched, preserving order.
	#[test]
	fn interleave_read_and_next_frame() {
		let mut producer = Info { sequence: 0 }.produce();
		for i in 0..5u8 {
			producer.write_frame_now(Bytes::from(vec![i; 1])).unwrap();
		}
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		// The first whole-frame read prefetches all five frames into the batch.
		let f0 = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0, Bytes::from(vec![0u8; 1]));

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
		producer.write_frame_now(Bytes::from_static(b"a")).unwrap();
		producer.write_frame_now(Bytes::from_static(b"b")).unwrap();

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
			producer.write_frame_now(Bytes::from_static(b"x")).unwrap();
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
		);
		let frame = frame::Info {
			size: 3,
			timestamp: Timestamp::from_millis(1).unwrap(), // 1ms -> 1000µs
		};
		let writer = producer.create_frame(frame).unwrap();
		assert_eq!(writer.timestamp.scale(), Timescale::MICRO);
		assert_eq!(writer.timestamp.value(), 1000);
	}

	/// `create_frame_now` stamps wall-clock now, at the group's scale.
	#[tokio::test]
	async fn create_frame_now_stamps_wall_clock() {
		use crate::Timescale;

		let mut producer = Producer::new(
			Info { sequence: 0 },
			track::Info::default().with_timescale(Timescale::MICRO),
		);
		let writer = producer.create_frame_now(3).unwrap();
		assert_eq!(writer.timestamp.scale(), Timescale::MICRO);
		assert!(!writer.timestamp.is_zero(), "wall clock should be non-zero");
	}

	/// The per-frame size cap (the group byte budget) is enforced before allocating.
	#[test]
	fn create_frame_rejects_oversized() {
		let mut producer = Info { sequence: 0 }.produce();
		let result = producer.create_frame_now(MAX_GROUP_CACHE + 1);
		assert!(matches!(result, Err(Error::FrameTooLarge)));
	}
}
