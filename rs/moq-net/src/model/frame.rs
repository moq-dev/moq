//! Frames are the leaf of the model: a sized, timestamped payload within a group.
//!
//! A group is a single ordered stream, so at most one frame is ever in flight.
//! Completed frames are plain data ([`Frame`]); the in-flight frame is written
//! through [`Producer`], which borrows its parent [`group::Producer`] exclusively so
//! the borrow checker enforces that only one frame is open at a time. A [`Consumer`]
//! reads one frame, sharing the group's channel rather than a per-frame one.
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::{Poll, ready};

use arrayvec::ArrayVec;
use bytes::Bytes;

use crate::group::{self, GroupState};
use crate::{Error, IntoBytes, Result, Timestamp, stats};

/// A chunk of data with an upfront size and a presentation timestamp.
///
/// This is just the header; the payload is carried separately (as a completed
/// [`Frame`] or streamed via [`Producer`] / [`Consumer`]).
#[derive(Clone, Copy, Debug)]
pub struct Info {
	/// Total payload size in bytes. Declared up front so consumers can preallocate.
	pub size: u64,
	/// Presentation timestamp.
	///
	/// [`group::Producer::create_frame`] converts it into the parent track's
	/// timescale, so the scale you build it with doesn't have to match the track.
	/// For data without a presentation time, pass [`Timestamp::now`] explicitly.
	pub timestamp: Timestamp,
}

/// A completed frame: a timestamp and its full, contiguous payload.
///
/// This is the stored form of every finished frame in a group. The payload is a
/// single [`Bytes`], so a consumer gets it with one zero-copy slice.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp, at the parent track's timescale.
	pub timestamp: Timestamp,
	/// The full frame payload.
	pub payload: Bytes,
}

/// A reusable batch of frames, filled by [`group::Consumer::read_frames`] and drained
/// by [`group::Producer::write_frames`].
///
/// A fixed-capacity inline buffer: `N` frames of stack storage, never a heap
/// allocation and never a spill. Allocate one per task and reuse it for the life of
/// the group rather than one per read.
///
/// `N` defaults to 8. Most reads are not big batches: at the live edge a frame arrives
/// at a time, so the capacity past the first frame or two is idle stack, and a
/// publisher holds one buffer per in-flight group. 8 costs 384 bytes and still reads
/// ~5x faster than a frame at a time (`benches/group.rs`). Ask for a larger `N` when
/// you know you are draining a backlog: 32 is ~8x, for 1.5 KB.
///
/// A default on a const parameter only applies in type position, so name the type to
/// get it: `let buf: frame::Buffer = Buffer::new()`.
///
/// A fill stamps the group's cache access once for the whole batch, which bounds
/// frames rather than elapsed time. A reader that may take longer than the track's
/// `max_age` to work through one batch calls
/// [`group::Consumer::keep_alive`] between frames, or the rest of the group is
/// expired out from under it.
#[derive(Debug, Default)]
pub struct Buffer<const N: usize = 8>(ArrayVec<Frame, N>);

impl<const N: usize> Buffer<N> {
	/// An empty buffer with room for `N` frames.
	pub fn new() -> Self {
		Self(ArrayVec::new())
	}

	/// How many frames a single fill can hold.
	pub const fn capacity(&self) -> usize {
		N
	}

	/// How many frames the buffer currently holds.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether the buffer holds no frames.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Whether the buffer is at capacity, so [`Self::push`] would refuse.
	pub fn is_full(&self) -> bool {
		self.0.is_full()
	}

	/// The frames from the most recent fill, in order.
	pub fn filled(&self) -> &[Frame] {
		&self.0
	}

	/// The frames from the most recent fill, mutably (to take payloads out, say).
	pub fn filled_mut(&mut self) -> &mut [Frame] {
		&mut self.0
	}

	/// Append a frame, handing it back if the buffer is already full.
	///
	/// Fill a buffer this way to hand a whole batch to
	/// [`group::Producer::write_frames`].
	pub fn push(&mut self, frame: Frame) -> std::result::Result<(), Frame> {
		self.0.try_push(frame).map_err(|err| err.element())
	}

	/// Move every frame out, leaving the buffer empty.
	///
	/// Frames left in the iterator when it drops are dropped with it, so the buffer
	/// ends up empty either way.
	pub fn drain(&mut self) -> impl ExactSizeIterator<Item = Frame> + '_ {
		self.0.drain(..)
	}

	/// Drop the current batch, leaving the buffer empty.
	pub fn clear(&mut self) {
		self.0.clear();
	}
}

/// Bytes one session may allocate up front for the frames it is still receiving.
///
/// A peer declares each frame's size before sending it, and allocating that size up
/// front saves a reallocation and copy per doubling as the payload arrives. The
/// declaration costs the peer nothing, though, so without a bound it could commit up to
/// [`group::MAX_CACHE_BYTES`] per stream without sending a byte. A frame whose declared
/// size fits what remains is allocated up front; past that, its buffer grows with the
/// bytes received. Each frame returns its share once it completes or aborts.
#[derive(Clone)]
pub(crate) struct Budget(Arc<AtomicUsize>);

impl Budget {
	/// Enough for a handful of large keyframes in flight at once.
	const DEFAULT: usize = 16 * 1024 * 1024;

	pub(crate) fn new(bytes: usize) -> Self {
		Self(Arc::new(AtomicUsize::new(bytes)))
	}

	/// Take `size` bytes of the budget, or `None` if that is more than remains.
	pub(crate) fn reserve(&self, size: usize) -> Option<Reservation> {
		self.0
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(size))
			.ok()?;
		Some(Reservation {
			budget: self.clone(),
			size,
		})
	}
}

impl Default for Budget {
	fn default() -> Self {
		Self::new(Self::DEFAULT)
	}
}

/// A frame's share of a [`Budget`], returned on drop.
pub(crate) struct Reservation {
	budget: Budget,
	size: usize,
}

impl Drop for Reservation {
	fn drop(&mut self) {
		self.budget.0.fetch_add(self.size, Ordering::Relaxed);
	}
}

/// Payload storage for the single in-flight frame, shared between the writing
/// [`Producer`] and any streaming [`Consumer`]s.
///
/// A whole-frame [`Bytes`] write is stored directly. Chunked writes are copied into a
/// heap [`Segment`], either sized to the declared frame up front or grown as bytes
/// arrive (see [`Budget`]). The producer writes through the raw pointer (sole writer,
/// guaranteed by the exclusive borrow of the parent group); `written` provides
/// happens-before for cross-thread reads.
#[derive(Clone)]
pub(crate) struct FrameBuf(Arc<FrameBufInner>);

struct FrameBufInner {
	size: usize,
	grow: bool,
	written: AtomicUsize,
	storage: OnceLock<FrameStorage>,
}

enum FrameStorage {
	Shared(Bytes),
	Fixed(Arc<Segment>),
	// Replaced by a larger copy whenever a write outgrows it. A reader clones the current
	// segment under the lock, so a slice it already handed out keeps the old one alive.
	Growing(Mutex<Arc<Segment>>),
}

/// An owned, zero-initialized heap allocation that chunked writes are copied into.
struct Segment {
	data: *mut u8,
	capacity: usize,
}

// Safety: `data` is owned (Box-allocated, freed in Drop). The producer is the sole
// writer and consumers only read bytes `< written`.
unsafe impl Send for Segment {}
unsafe impl Sync for Segment {}

impl Drop for Segment {
	fn drop(&mut self) {
		// Safety: data was obtained from `Box::into_raw` of a `Box<[u8]>` of length
		// `capacity` and is not aliased at drop (Arc refcount hit 0).
		unsafe {
			let slice = std::ptr::slice_from_raw_parts_mut(self.data, self.capacity);
			drop(Box::from_raw(slice));
		}
	}
}

impl Segment {
	fn new(capacity: usize) -> Self {
		let boxed: Box<[u8]> = vec![0u8; capacity].into_boxed_slice();
		let capacity = boxed.len();
		let data = Box::into_raw(boxed) as *mut u8;
		Self { data, capacity }
	}

	/// Copy `src` in at `offset`.
	///
	/// Safety: caller must be the sole writer, `offset + src.len()` must be within
	/// `capacity`, and no reader may have been handed bytes at or past `offset`.
	unsafe fn write(&self, offset: usize, src: &[u8]) {
		debug_assert!(offset + src.len() <= self.capacity);
		unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), self.data.add(offset), src.len()) };
	}
}

/// The first `len` bytes of a [`Segment`], all written, so it can back a
/// [`Bytes::from_owner`].
struct Filled {
	segment: Arc<Segment>,
	len: usize,
}

impl AsRef<[u8]> for Filled {
	fn as_ref(&self) -> &[u8] {
		// Safety: `len` never exceeds what the producer had published when this was
		// built, and the producer never rewrites published bytes. The Arc keeps the
		// allocation alive while any reference to the slice lives.
		unsafe { std::slice::from_raw_parts(self.segment.data, self.len) }
	}
}

impl FrameBuf {
	/// A buffer for a frame of `size` bytes, allocated at that size on the first write.
	///
	/// The oversized-frame guard lives in [`group::Producer`], which rejects a declared
	/// size larger than the group's byte budget before calling this.
	pub(crate) fn new(size: usize) -> Self {
		Self::with_growth(size, false)
	}

	/// A buffer for a frame of `size` bytes that allocates only as bytes arrive.
	pub(crate) fn growing(size: usize) -> Self {
		Self::with_growth(size, true)
	}

	fn with_growth(size: usize, grow: bool) -> Self {
		Self(Arc::new(FrameBufInner {
			size,
			grow,
			written: AtomicUsize::new(0),
			storage: OnceLock::new(),
		}))
	}

	/// The frame's declared size.
	pub(crate) fn size(&self) -> usize {
		self.0.size
	}

	pub(crate) fn written(&self, ord: Ordering) -> usize {
		self.0.written.load(ord)
	}

	fn try_set_bytes(&self, bytes: Bytes) -> std::result::Result<(), Bytes> {
		if bytes.len() != self.size() || self.written(Ordering::Acquire) != 0 {
			return Err(bytes);
		}
		self.0
			.storage
			.set(FrameStorage::Shared(bytes))
			.map_err(|storage| match storage {
				FrameStorage::Shared(bytes) => bytes,
				_ => unreachable!("try_set_bytes only installs shared storage"),
			})
	}

	/// Safety: caller must be the sole producer and `new_written` must be `<= size`.
	unsafe fn store_written(&self, new_written: usize) {
		// Release pairs with consumers' Acquire load to publish prior writes.
		self.0.written.store(new_written, Ordering::Release);
	}

	/// Append `src` at the current write offset and publish it.
	///
	/// Safety relies on the single-producer invariant: only one [`Producer`] exists for
	/// a frame (it holds the exclusive borrow of the parent group), so this is the sole
	/// writer even though it takes `&self`.
	fn append(&self, src: &[u8]) {
		if src.is_empty() {
			return;
		}
		let prev = self.written(Ordering::Relaxed);
		let storage = self.0.storage.get_or_init(|| match self.0.grow {
			true => FrameStorage::Growing(Mutex::new(Arc::new(Segment::new(0)))),
			false => FrameStorage::Fixed(Arc::new(Segment::new(self.size()))),
		});
		// Safety (every write below): sole writer; the caller bounds-checked `src` against
		// the remaining size, and consumers only read `[..written]`.
		match storage {
			// Only reachable if the frame is already complete via shared storage, which
			// `Producer::write` rejects for a non-empty chunk. Nothing to copy.
			FrameStorage::Shared(_) => return,
			FrameStorage::Fixed(segment) => unsafe { segment.write(prev, src) },
			FrameStorage::Growing(current) => {
				let mut current = current.lock().expect("mutex poisoned");
				let needed = prev + src.len();
				if current.capacity < needed {
					// Doubling keeps the copies linear in the frame size, and the cap makes
					// the last segment exactly the frame, so it freezes without a copy.
					let next = Segment::new(needed.max(current.capacity * 2).min(self.size()));
					let filled = Filled {
						segment: current.clone(),
						len: prev,
					};
					unsafe { next.write(0, filled.as_ref()) };
					*current = Arc::new(next);
				}
				unsafe { current.write(prev, src) };
			}
		}
		// Safety: sole writer, and the caller bounds-checked `src` against the size.
		unsafe { self.store_written(prev + src.len()) };
	}

	/// Freeze the buffer into the completed payload.
	///
	/// Returns the shared [`Bytes`] directly for a whole-frame write (zero-copy), or
	/// wraps the mutable allocation otherwise.
	fn freeze(&self) -> Bytes {
		self.slice(0, self.size())
	}

	/// A zero-copy slice of the written region `[start..end]`.
	///
	/// `end` must not exceed a `written` value already loaded with `Acquire`, which is
	/// also what makes the segment read here at least as new as those bytes.
	fn slice(&self, start: usize, end: usize) -> Bytes {
		let segment = match self.0.storage.get() {
			Some(FrameStorage::Shared(bytes)) => return bytes.slice(start..end),
			Some(FrameStorage::Fixed(segment)) => segment.clone(),
			Some(FrameStorage::Growing(current)) => current.lock().expect("mutex poisoned").clone(),
			// Nothing written, so the range is empty.
			None => return Bytes::new(),
		};
		debug_assert!(end <= segment.capacity);
		Bytes::from_owner(Filled { segment, len: end }).slice(start..)
	}

	/// Heap bytes the buffer currently holds for the payload.
	#[cfg(test)]
	pub(crate) fn allocated(&self) -> usize {
		match self.0.storage.get() {
			Some(FrameStorage::Shared(bytes)) => bytes.len(),
			Some(FrameStorage::Fixed(segment)) => segment.capacity,
			Some(FrameStorage::Growing(current)) => current.lock().expect("mutex poisoned").capacity,
			None => 0,
		}
	}
}

/// The writer behind [`Producer`] and [`ProducerOwned`], generic over how it
/// reaches the parent group (an exclusive borrow, or an owned clone).
struct Raw<G: std::borrow::BorrowMut<group::Producer>> {
	group: G,
	buf: FrameBuf,
	info: Info,
	// Set once the frame is committed (finished) or aborted, so Drop is a no-op.
	done: bool,
	// Ingress payload meter, inherited from the parent group. Counts each written
	// chunk's bytes. Empty (no-op) for an untagged group.
	stats: stats::Meter,
}

impl<G: std::borrow::BorrowMut<group::Producer>> Raw<G> {
	fn remaining(&self) -> usize {
		self.buf.size() - self.buf.written(Ordering::Acquire)
	}

	fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		let len = chunk.as_ref().len();
		if len > self.remaining() {
			return Err(Error::WrongSize);
		}
		// Ingress payload: count the chunk's bytes as they're written.
		self.stats.bytes(len as u64);
		// Fast path: a single whole-frame write keeps the caller's allocation.
		if len == self.buf.size() && self.buf.written(Ordering::Acquire) == 0 {
			match self.buf.try_set_bytes(chunk.into_bytes()) {
				Ok(()) => {
					let size = self.buf.size();
					// Safety: `try_set_bytes` checked the buffer exactly matches the declared
					// size, so publishing all bytes is in bounds.
					unsafe { self.buf.store_written(size) };
				}
				Err(chunk) => self.buf.append(&chunk),
			}
		} else {
			self.buf.append(chunk.as_ref());
		}
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if self.buf.written(Ordering::Acquire) != self.buf.size() {
			return Err(Error::WrongSize);
		}
		let payload = self.buf.freeze();
		self.group.borrow_mut().frame_commit(Frame {
			timestamp: self.info.timestamp,
			payload,
		})?;
		self.done = true;
		Ok(())
	}

	fn abort(&mut self, err: Error) -> Result<()> {
		self.group.borrow_mut().frame_abort(err);
		self.done = true;
		Ok(())
	}
}

impl<G: std::borrow::BorrowMut<group::Producer>> Drop for Raw<G> {
	fn drop(&mut self) {
		if !self.done {
			// An unfinished frame leaves the group stream broken; fail the group so
			// consumers surface an error instead of hanging on the partial forever.
			// A group already aborted (superseded, evicted, cancelled) carries its own
			// reason, so cutting its in-flight frame short is expected.
			let group = self.group.borrow_mut();
			if !group.is_aborted() {
				tracing::warn!(
					group = group.info().sequence,
					"frame::Producer dropped before writing all bytes"
				);
			}
			group.frame_abort(Error::Dropped);
		}
	}
}

/// Writes the payload of the single in-flight frame in one or more chunks.
///
/// Borrows the parent [`group::Producer`] exclusively, so no other frame can be
/// opened while this one is live. The total bytes written must exactly match
/// [`Info::size`]; call [`Self::finish`] to commit the frame (or [`Self::abort`] to
/// fail it). Dropping without either aborts the group, since an unfinished frame
/// leaves the group's stream broken.
///
/// A single whole-frame [`write`](Self::write) keeps the caller's allocation
/// (zero-copy); chunked writes copy into one buffer sized to the declared frame.
pub struct Producer<'a>(Raw<&'a mut group::Producer>);

impl std::ops::Deref for Producer<'_> {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.0.info
	}
}

impl<'a> Producer<'a> {
	pub(crate) fn new(group: &'a mut group::Producer, buf: FrameBuf, info: Info) -> Self {
		Self(Raw {
			group,
			buf,
			info,
			done: false,
			stats: stats::Meter::default(),
		})
	}

	/// Attach the parent group's ingress meter, so written chunks bump `bytes`.
	pub(crate) fn with_meter(mut self, meter: stats::Meter) -> Self {
		self.0.stats = meter;
		self
	}

	/// The parent group this frame belongs to.
	pub fn group(&self) -> group::Info {
		self.0.group.info()
	}

	/// Bytes still needed to complete the frame.
	pub fn remaining(&self) -> usize {
		self.0.remaining()
	}

	/// Write a chunk of data to the frame.
	///
	/// Returns [`Error::WrongSize`] if the chunk would exceed the remaining bytes.
	pub fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		self.0.write(chunk)?;
		self.0.group.frame_notify();
		Ok(())
	}

	/// Commit the frame, verifying that all bytes were written.
	///
	/// Returns [`Error::WrongSize`] if the bytes written don't match [`Info::size`].
	pub fn finish(mut self) -> Result<()> {
		self.0.finish()
	}

	/// Abort the frame (and its group) with the given error.
	pub fn abort(mut self, err: Error) -> Result<()> {
		self.0.abort(err)
	}
}

/// The owned counterpart of [`Producer`], for the wire drivers that stream a
/// frame across polls and cannot hold the group borrowed inside their state.
///
/// Crate-private on purpose: the exclusivity the public borrow enforces (one
/// live frame per group) becomes the holder's promise here. Do not open another
/// frame on the group until this one is finished or aborted.
///
/// Holds the frame's share of the session's [`Budget`], if it got one, until then.
pub(crate) struct ProducerOwned {
	raw: Raw<group::Producer>,
	_reserved: Option<Reservation>,
}

impl std::ops::Deref for ProducerOwned {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.raw.info
	}
}

impl ProducerOwned {
	pub(crate) fn new(group: group::Producer, buf: FrameBuf, info: Info, reserved: Option<Reservation>) -> Self {
		Self {
			raw: Raw {
				group,
				buf,
				info,
				done: false,
				stats: stats::Meter::default(),
			},
			_reserved: reserved,
		}
	}

	/// Attach the parent group's ingress meter, so written chunks bump `bytes`.
	pub(crate) fn with_meter(mut self, meter: stats::Meter) -> Self {
		self.raw.stats = meter;
		self
	}

	/// Bytes still needed to complete the frame.
	pub fn remaining(&self) -> usize {
		self.raw.remaining()
	}

	/// Write a chunk of payload *without* waking consumers; pair it with [`Self::notify`].
	///
	/// The wake is split out because the wire ingest drains every chunk the transport
	/// has already buffered in one poll turn, and a consumer parked on the group cannot
	/// run until that turn yields. Waking per chunk pays a group lock and a clock read
	/// to publish bytes nobody can observe yet.
	///
	/// `coding::Reader::poll_read_frame` owns the pairing and is the only caller.
	pub(crate) fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		self.raw.write(chunk)
	}

	/// Publish what has been written so far, waking consumers parked on the group.
	pub(crate) fn notify(&self) {
		self.raw.group.frame_notify();
	}

	/// Commit the frame, verifying that all bytes were written.
	pub fn finish(mut self) -> Result<()> {
		self.raw.finish()
	}

	/// Abort the frame (and its group) with the given error.
	pub fn abort(mut self, err: Error) -> Result<()> {
		self.raw.abort(err)
	}

	/// Heap bytes the frame's buffer currently holds for the payload.
	#[cfg(test)]
	pub(crate) fn allocated(&self) -> usize {
		self.raw.buf.allocated()
	}
}

/// The source of a [`Consumer`]'s payload: a finished frame (whole) or the in-flight
/// tail (streamed).
#[derive(Clone)]
pub(crate) enum Source {
	Complete(Bytes),
	Partial(FrameBuf),
}

/// Subscriber expiry state carried across the group-to-frame handoff.
#[derive(Clone)]
pub(crate) struct Expiry {
	policy: Arc<dyn group::Expiry>,
	stale_stats: stats::Meter,
	stale_counted: Arc<AtomicBool>,
	tail: Range<usize>,
	count_payload: bool,
}

impl Expiry {
	pub(crate) fn new(
		policy: Arc<dyn group::Expiry>,
		stale_stats: stats::Meter,
		stale_counted: Arc<AtomicBool>,
	) -> Self {
		Self {
			policy,
			stale_stats,
			stale_counted,
			tail: 0..0,
			count_payload: false,
		}
	}

	pub(crate) fn for_frame(mut self, tail: Range<usize>, count_payload: bool) -> Self {
		self.tail = tail;
		self.count_payload = count_payload;
		self
	}
}

/// Reads one frame's payload, streaming as bytes arrive for the in-flight tail.
///
/// Owns a handle to the parent group's channel (not a per-frame one), so a group with
/// many frames doesn't allocate a channel per frame. Cloning yields an independent
/// reader of the same frame.
#[derive(Clone)]
pub struct Consumer {
	// The group's channel, used to park while a partial frame fills.
	state: kio::Consumer<GroupState>,
	info: Info,
	source: Source,
	// Byte offset consumed so far.
	read_idx: usize,
	// Egress payload meter, so chunks bump `bytes` exactly once as they're read out.
	// Empty (no-op) for an untagged group.
	stats: stats::Meter,
	// The parent subscription can expire after this frame handle is returned.
	expiry: Option<Expiry>,
	expired: bool,
	// Read from a front's logical track: carries the read across route changes, with
	// this frame's index in its group. Boxed: it is the rare case.
	recover: Option<Box<(super::resume::Recover, u64)>>,
}

impl std::ops::Deref for Consumer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Consumer {
	pub(crate) fn new(state: kio::Consumer<GroupState>, info: Info, source: Source) -> Self {
		Self {
			state,
			info,
			source,
			read_idx: 0,
			stats: stats::Meter::default(),
			expiry: None,
			expired: false,
			recover: None,
		}
	}

	/// Carry this frame across a front's route changes; see [`super::resume`].
	pub(crate) fn with_recover(mut self, recover: super::resume::Recover, index: u64) -> Self {
		self.recover = Some(Box::new((recover, index)));
		self
	}

	/// Run `read`, and once this copy fails with its route, or stalls while a newer route
	/// serves, continue from the serving route's copy of the same frame, past the bytes
	/// already read.
	fn poll_resumed<T>(
		&mut self,
		waiter: &kio::Waiter,
		mut read: impl FnMut(&mut Self, &kio::Waiter) -> Poll<Result<T>>,
	) -> Poll<Result<T>> {
		loop {
			let res = read(self, waiter);
			if self.expired {
				return res;
			}
			let Some(recover) = self.recover.as_mut() else {
				return res;
			};
			let failed = match &res {
				Poll::Ready(Ok(_)) => return res,
				Poll::Ready(Err(err)) => Some(err.clone()),
				Poll::Pending => None,
			};
			let (recover, index) = &mut **recover;
			if !recover.wants(failed.as_ref()) {
				return res;
			}
			let mut group = match recover.poll(*index, failed.as_ref(), waiter) {
				Poll::Ready(Ok(group)) => group,
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => return Poll::Pending,
			};
			// The copy holds the frame's header at least, or it would not have been adopted.
			let frame = match group.poll_next_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => frame,
				Poll::Ready(Ok(None)) => return Poll::Ready(Err(Error::WrongSize)),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => return Poll::Pending,
			};
			// Same name, same content: a different size is the routes disagreeing.
			if frame.info.size != self.info.size {
				return Poll::Ready(Err(Error::ProtocolViolation));
			}
			self.state = frame.state;
			self.source = frame.source;
		}
	}

	/// Attach an egress meter so read-out chunks bump `bytes`. Used only for frames
	/// read directly from the group (whose bytes weren't counted at a batch fill).
	pub(crate) fn with_meter(mut self, meter: stats::Meter) -> Self {
		self.stats = meter;
		self
	}

	pub(crate) fn with_expiry(mut self, expiry: Expiry) -> Self {
		self.expiry = Some(expiry);
		self
	}

	fn size(&self) -> usize {
		match &self.source {
			Source::Complete(bytes) => bytes.len(),
			Source::Partial(_) => self.info.size as usize,
		}
	}

	/// Evaluate the parent subscription's drift budget.
	///
	/// Only called once a read has nothing buffered to return: the budget bounds a
	/// *stalled* payload, so bytes already in hand are always drained rather than
	/// truncated. `self.expired` is sticky, so the answer is only ever computed once.
	fn poll_expired(&mut self, waiter: &kio::Waiter) -> bool {
		if self.expired || self.read_idx >= self.size() {
			return self.expired;
		}
		let Some(expiry) = &self.expiry else {
			return false;
		};
		if !expiry.policy.is_expired(waiter) {
			return false;
		}

		self.expired = true;
		if !expiry.stale_counted.swap(true, Ordering::Relaxed) {
			let mut stale = self.state.read().content_range(expiry.tail.start, expiry.tail.end);
			if expiry.count_payload {
				stale.bytes += self.size().saturating_sub(self.read_idx) as u64;
			}
			expiry.stale_stats.stale(stale);
		}
		true
	}

	/// Poll for the next chunk of bytes since the last read.
	///
	/// Returns `None` once the frame is finished and all bytes have been consumed.
	pub fn poll_read_chunk(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		if self.recover.is_none() {
			return self.poll_read_chunk_once(waiter);
		}
		self.poll_resumed(waiter, Self::poll_read_chunk_once)
	}

	fn poll_read_chunk_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		if self.expired {
			return Poll::Ready(Err(Error::Old));
		}
		let buf = match &self.source {
			Source::Complete(bytes) => {
				if self.read_idx >= bytes.len() {
					return Poll::Ready(Ok(None));
				}
				let out = bytes.slice(self.read_idx..);
				self.read_idx = bytes.len();
				self.stats.bytes(out.len() as u64);
				return Poll::Ready(Ok(Some(out)));
			}
			Source::Partial(buf) => buf.clone(),
		};

		let size = self.info.size as usize;
		loop {
			let written = buf.written(Ordering::Acquire);
			if written > self.read_idx {
				let out = buf.slice(self.read_idx, written);
				self.read_idx = written;
				self.stats.bytes(out.len() as u64);
				return Poll::Ready(Ok(Some(out)));
			}
			if written >= size {
				return Poll::Ready(Ok(None));
			}
			// Nothing buffered and the frame isn't finished: this park is the stall the
			// drift budget exists to bound, and the only place it can apply.
			if self.poll_expired(waiter) {
				return Poll::Ready(Err(Error::Old));
			}
			let read_idx = self.read_idx;
			// Park on the group's channel; the producer notifies it on each write and
			// on abort. Re-check the atomic on wake.
			ready!(poll_state(&self.state, waiter, |state| {
				if let Some(err) = &state.abort {
					return Poll::Ready(Err(err.clone()));
				}
				let w = buf.written(Ordering::Acquire);
				if w > read_idx || w >= size {
					Poll::Ready(Ok(()))
				} else {
					Poll::Pending
				}
			})?);
		}
	}

	/// Return the next chunk of bytes since the last read.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_read_chunk(waiter)).await
	}

	/// Poll for all remaining bytes, resolving once the frame is finished.
	pub fn poll_read_all(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes>> {
		if self.recover.is_none() {
			return self.poll_read_all_once(waiter);
		}
		self.poll_resumed(waiter, Self::poll_read_all_once)
	}

	fn poll_read_all_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes>> {
		if self.expired {
			return Poll::Ready(Err(Error::Old));
		}
		let buf = match &self.source {
			Source::Complete(bytes) => {
				let out = bytes.slice(self.read_idx..);
				self.read_idx = bytes.len();
				self.stats.bytes(out.len() as u64);
				return Poll::Ready(Ok(out));
			}
			Source::Partial(buf) => buf.clone(),
		};

		let size = self.info.size as usize;
		let read_idx = self.read_idx;
		// Waiting on the rest of the payload is a stall; see `poll_read_chunk`.
		if buf.written(Ordering::Acquire) < size && self.poll_expired(waiter) {
			return Poll::Ready(Err(Error::Old));
		}
		ready!(poll_state(&self.state, waiter, |state| {
			if let Some(err) = &state.abort {
				return Poll::Ready(Err(err.clone()));
			}
			if buf.written(Ordering::Acquire) >= size {
				Poll::Ready(Ok(()))
			} else {
				Poll::Pending
			}
		})?);
		let out = buf.slice(read_idx, size);
		self.read_idx = size;
		self.stats.bytes(out.len() as u64);
		Poll::Ready(Ok(out))
	}

	/// Return all remaining bytes, blocking until the frame is finished.
	pub async fn read_all(&mut self) -> Result<Bytes> {
		kio::wait(|waiter| self.poll_read_all(waiter)).await
	}
}

/// Poll the group channel, mapping a terminal close without an error to
/// [`Error::Dropped`]. Mirrors [`group::Consumer`]'s internal helper.
fn poll_state<F, R>(state: &kio::Consumer<GroupState>, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
where
	F: Fn(&kio::Ref<'_, GroupState>) -> Poll<Result<R>>,
{
	Poll::Ready(match ready!(state.poll(waiter, f)) {
		Ok(res) => res,
		Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Each reallocation leaves the slices already handed out intact, and the last
	/// segment is exactly the frame, so the payload freezes without another copy.
	#[test]
	fn growing_buffer_keeps_handed_out_slices() {
		let buf = FrameBuf::growing(10);
		buf.append(b"abc");
		assert_eq!(buf.allocated(), 3);
		let early = buf.slice(0, 3);

		buf.append(b"defg");
		assert_eq!(buf.allocated(), 7);
		buf.append(b"hij");
		assert_eq!(buf.allocated(), 10, "doubling stops at the declared size");

		assert_eq!(early, &b"abc"[..]);
		assert_eq!(buf.slice(3, 7), &b"defg"[..]);
		assert_eq!(buf.freeze(), &b"abcdefghij"[..]);
	}

	/// A reader on another thread slicing while the writer reallocates only ever sees
	/// the bytes that were written.
	#[test]
	fn growing_buffer_reads_across_threads() {
		const SIZE: usize = 256 * 1024;
		let buf = FrameBuf::growing(SIZE);
		let reader = std::thread::spawn({
			let buf = buf.clone();
			move || {
				let mut read = 0;
				while read < SIZE {
					let written = buf.written(Ordering::Acquire);
					let chunk = buf.slice(read, written);
					for (i, byte) in chunk.iter().enumerate() {
						assert_eq!(*byte, ((read + i) % 251) as u8);
					}
					read = written;
					std::thread::yield_now();
				}
			}
		});

		let payload: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
		for chunk in payload.chunks(1000) {
			buf.append(chunk);
		}
		reader.join().unwrap();
		assert_eq!(buf.freeze(), payload);
	}

	/// A reservation holds its bytes until it drops, and a request larger than what
	/// remains takes nothing.
	#[test]
	fn budget_returns_reservations() {
		let budget = Budget::new(100);
		let a = budget.reserve(60).unwrap();
		assert!(budget.reserve(41).is_none());
		let b = budget.reserve(40).unwrap();
		assert!(budget.reserve(1).is_none());

		drop(a);
		drop(b);
		assert!(budget.reserve(100).is_some());
	}
}
