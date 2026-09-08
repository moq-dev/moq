//! Frames are the leaf of the model: a sized, timestamped payload within a group.
//!
//! A group is a single ordered stream, so at most one frame is ever in flight.
//! Completed frames are plain data ([`Frame`]); the in-flight frame is written
//! through [`Producer`], which borrows its parent [`group::Producer`] exclusively so
//! the borrow checker enforces that only one frame is open at a time. A [`Consumer`]
//! reads one frame, sharing the group's channel rather than a per-frame one.
use std::ops::Range;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Payload storage for the single in-flight frame, shared between the writing
/// [`Producer`] and any streaming [`Consumer`]s.
///
/// A whole-frame [`Bytes`] write is stored directly. Chunked writes reserve a
/// contiguous region from the group's adaptive pages. The producer writes through
/// the raw pointer (sole writer, guaranteed by the exclusive borrow of the parent
/// group); `written` provides happens-before for cross-thread reads. Implements
/// [AsRef]<[u8]> so it can back a [`Bytes::from_owner`].
#[derive(Clone)]
pub(crate) struct FrameBuf(Arc<FrameBufInner>);

struct FrameBufInner {
	capacity: usize,
	written: AtomicUsize,
	storage: OnceLock<FrameStorage>,
}

enum FrameStorage {
	Shared(Bytes),
	Mutable(MutableFrameBuf),
}

struct Page {
	// Owned heap allocation of `capacity` bytes (zero-initialized).
	data: *mut u8,
	capacity: usize,
}

// Safety: the Box lives until the last page reference drops. Reservations never
// overlap, each frame has one writer, and readers only access that frame's
// published prefix after an Acquire load of its written count.
unsafe impl Send for Page {}
unsafe impl Sync for Page {}

impl Drop for Page {
	fn drop(&mut self) {
		// Safety: data was obtained from `Box::into_raw` of a `Box<[u8]>` of length
		// `capacity` and is not aliased at drop (Arc refcount hit 0).
		unsafe {
			let slice = std::ptr::slice_from_raw_parts_mut(self.data, self.capacity);
			drop(Box::from_raw(slice));
		}
	}
}

impl Page {
	fn new(size: usize) -> Self {
		let boxed: Box<[u8]> = vec![0u8; size].into_boxed_slice();
		let capacity = boxed.len();
		let data = Box::into_raw(boxed) as *mut u8;
		Self { data, capacity }
	}
}

/// Reserves disjoint contiguous frame regions, growing small-frame pages up to 64 KiB.
#[derive(Default)]
pub(crate) struct Pages {
	page: Option<Arc<Page>>,
	offset: usize,
	previous: usize,
}

impl Pages {
	fn allocate(&mut self, size: usize) -> MutableFrameBuf {
		const MAX: usize = 64 * 1024;
		// Large frames must remain contiguous without making later small frames
		// retain a large allocation. They do not consume or grow the shared tail.
		if size > MAX {
			return MutableFrameBuf::Owned(Page::new(size));
		}
		if self.page.as_ref().is_none_or(|page| page.capacity - self.offset < size) {
			let capacity = size.max(128).next_power_of_two().max(self.previous * 2).min(MAX);
			self.page = Some(Arc::new(Page::new(capacity)));
			self.offset = 0;
			self.previous = capacity;
		}
		let page = self.page.as_ref().unwrap().clone();
		let offset = self.offset;
		self.offset += size;
		MutableFrameBuf::Shared { page, offset }
	}
}

/// A frame's exclusive reservation within a shared allocation.
enum MutableFrameBuf {
	Shared { page: Arc<Page>, offset: usize },
	Owned(Page),
}

impl MutableFrameBuf {
	fn ptr(&self) -> *mut u8 {
		match self {
			Self::Shared { page, offset } => page.data.wrapping_add(*offset),
			Self::Owned(page) => page.data,
		}
	}
}

impl FrameBuf {
	/// Allocate a buffer for a frame of `size` bytes.
	///
	/// The oversized-frame guard lives in [`group::Producer`], which rejects a declared
	/// size larger than the group's byte budget before calling this.
	pub(crate) fn new(size: usize) -> Self {
		Self(Arc::new(FrameBufInner {
			capacity: size,
			written: AtomicUsize::new(0),
			storage: OnceLock::new(),
		}))
	}

	pub(crate) fn capacity(&self) -> usize {
		self.0.capacity
	}

	pub(crate) fn written(&self, ord: Ordering) -> usize {
		self.0.written.load(ord)
	}

	fn try_set_bytes(&self, bytes: Bytes) -> std::result::Result<(), Bytes> {
		if bytes.len() != self.capacity() || self.written(Ordering::Acquire) != 0 {
			return Err(bytes);
		}
		self.0
			.storage
			.set(FrameStorage::Shared(bytes))
			.map_err(|storage| match storage {
				FrameStorage::Shared(bytes) => bytes,
				FrameStorage::Mutable(_) => unreachable!("try_set_bytes only installs shared storage"),
			})
	}

	/// Reserve storage only when a nonempty write cannot keep an owned buffer.
	fn mutable(&self, pages: &mut Pages) -> &MutableFrameBuf {
		let storage = self
			.0
			.storage
			.get_or_init(|| FrameStorage::Mutable(pages.allocate(self.capacity())));
		let FrameStorage::Mutable(buf) = storage else {
			unreachable!("a completed shared frame cannot accept more bytes");
		};
		buf
	}

	/// Safety: caller must be the sole producer and `new_written` must be `<= capacity`.
	unsafe fn store_written(&self, new_written: usize) {
		// Release pairs with consumers' Acquire load to publish prior writes.
		self.0.written.store(new_written, Ordering::Release);
	}

	/// Append `src` at the current write offset and publish it.
	///
	/// Safety relies on the single-producer invariant: only one [`Producer`] exists for
	/// a frame (it holds the exclusive borrow of the parent group), so this is the sole
	/// writer even though it takes `&self`.
	fn append(&self, src: &[u8], pages: &mut Pages) {
		if src.is_empty() {
			return;
		}
		let prev = self.written(Ordering::Relaxed);
		let buf = self.mutable(pages);
		// Safety: sole writer; the caller bounds-checked `src` against the remaining
		// capacity, and consumers only read `[..written]`.
		unsafe {
			std::ptr::copy_nonoverlapping(src.as_ptr(), buf.ptr().add(prev), src.len());
			self.store_written(prev + src.len());
		}
	}

	/// Freeze the buffer into the completed payload (`size` bytes).
	///
	/// Returns the shared [`Bytes`] directly for a whole-frame write (zero-copy), or
	/// wraps the mutable allocation otherwise.
	fn freeze(&self, size: usize) -> Bytes {
		match self.0.storage.get() {
			Some(FrameStorage::Shared(bytes)) => bytes.clone(),
			_ => self.slice(0, size),
		}
	}

	/// A zero-copy slice of the initialized region `[start..end]`.
	fn slice(&self, start: usize, end: usize) -> Bytes {
		Bytes::from_owner(self.clone()).slice(start..end)
	}
}

impl AsRef<[u8]> for FrameBuf {
	fn as_ref(&self) -> &[u8] {
		// Snapshot the initialized region (bytes the producer has written so far).
		// Acquire pairs with the producer's Release on `written`.
		let written = self.0.written.load(Ordering::Acquire);
		match self.0.storage.get() {
			Some(FrameStorage::Shared(bytes)) => &bytes[..written],
			Some(FrameStorage::Mutable(buf)) => {
				// Safety: data..data+written is initialized (zero-init at alloc + producer
				// writes up to `written`). The Arc keeps the allocation alive while any
				// reference to the slice lives.
				unsafe { std::slice::from_raw_parts(buf.ptr(), written) }
			}
			None => &[],
		}
	}
}

/// The writer behind [`Producer`] and [`ProducerOwned`], generic over how it
/// reaches the parent group (an exclusive borrow, or an owned clone).
struct Raw<G: std::borrow::BorrowMut<group::Producer>> {
	group: G,
	buf: FrameBuf,
	pages: Pages,
	info: Info,
	// Set once the frame is committed (finished) or aborted, so Drop is a no-op.
	done: bool,
	// Ingress payload meter, inherited from the parent group. Counts each written
	// chunk's bytes. Empty (no-op) for an untagged group.
	stats: stats::Meter,
}

impl<G: std::borrow::BorrowMut<group::Producer>> Raw<G> {
	fn remaining(&self) -> usize {
		self.buf.capacity() - self.buf.written(Ordering::Acquire)
	}

	fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		let len = chunk.as_ref().len();
		if len > self.remaining() {
			return Err(Error::WrongSize);
		}
		// Ingress payload: count the chunk's bytes as they're written.
		self.stats.bytes(len as u64);
		// Fast path: a single whole-frame write keeps the caller's allocation.
		if len == self.buf.capacity() && self.buf.written(Ordering::Acquire) == 0 {
			match self.buf.try_set_bytes(chunk.into_bytes()) {
				Ok(()) => {
					let cap = self.buf.capacity();
					// Safety: `try_set_bytes` checked the buffer exactly matches the declared
					// size, so publishing all bytes is in bounds.
					unsafe { self.buf.store_written(cap) };
				}
				Err(chunk) => self.buf.append(&chunk, &mut self.pages),
			}
		} else {
			self.buf.append(chunk.as_ref(), &mut self.pages);
		}
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if self.buf.written(Ordering::Acquire) != self.buf.capacity() {
			return Err(Error::WrongSize);
		}
		let payload = self.buf.freeze(self.buf.capacity());
		self.group.borrow_mut().frame_commit(
			Frame {
				timestamp: self.info.timestamp,
				payload,
			},
			std::mem::take(&mut self.pages),
		)?;
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
			tracing::warn!(
				group = self.group.borrow_mut().info().sequence,
				"frame::Producer dropped before writing all bytes"
			);
			self.group.borrow_mut().frame_abort(Error::Dropped);
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
	pub(crate) fn new(group: &'a mut group::Producer, buf: FrameBuf, info: Info, pages: Pages) -> Self {
		Self(Raw {
			group,
			buf,
			pages,
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
pub(crate) struct ProducerOwned(Raw<group::Producer>);

impl std::ops::Deref for ProducerOwned {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.0.info
	}
}

impl ProducerOwned {
	pub(crate) fn new(group: group::Producer, buf: FrameBuf, info: Info, pages: Pages) -> Self {
		Self(Raw {
			group,
			buf,
			pages,
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

	/// Bytes still needed to complete the frame.
	pub fn remaining(&self) -> usize {
		self.0.remaining()
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
		self.0.write(chunk)
	}

	/// Publish what has been written so far, waking consumers parked on the group.
	pub(crate) fn notify(&self) {
		self.0.group.frame_notify();
	}

	/// Commit the frame, verifying that all bytes were written.
	pub fn finish(mut self) -> Result<()> {
		self.0.finish()
	}

	/// Abort the frame (and its group) with the given error.
	pub fn abort(mut self, err: Error) -> Result<()> {
		self.0.abort(err)
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

	#[test]
	fn page_reservations_grow_without_overlapping() {
		fn shared(buf: MutableFrameBuf) -> (Arc<Page>, usize) {
			let MutableFrameBuf::Shared { page, offset } = buf else {
				panic!("small frames should share a page");
			};
			(page, offset)
		}
		let mut pages = Pages::default();
		assert!(pages.page.is_none());
		let (first, first_offset) = shared(pages.allocate(64));
		assert_eq!(first.capacity, 128);
		let (second, second_offset) = shared(pages.allocate(64));
		assert!(Arc::ptr_eq(&first, &second));
		assert_eq!(second_offset, first_offset + 64);
		let (third, _) = shared(pages.allocate(64));
		assert_eq!(third.capacity, 256);
		assert!(!Arc::ptr_eq(&second, &third));
		let MutableFrameBuf::Owned(large) = pages.allocate(65537) else {
			panic!("large frames should own a dedicated allocation");
		};
		assert_eq!(large.capacity, 65537);
		let (fourth, _) = shared(pages.allocate(64));
		assert!(Arc::ptr_eq(&third, &fourth));
		for _ in 0..100 {
			assert_eq!(shared(pages.allocate(65536)).0.capacity, 65536);
		}
		let retained = Arc::downgrade(&first);
		drop(pages);
		drop(first);
		assert!(retained.upgrade().is_some());
		drop(second);
		assert!(retained.upgrade().is_none());
	}
}
