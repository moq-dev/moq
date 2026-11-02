//! Frames are the leaf of the model: a sized, optionally timestamped payload within a group.
//!
//! A group is a single ordered stream, so at most one frame is ever in flight.
//! Completed frames are plain data ([`Frame`]); the in-flight frame is written
//! through [`Producer`], which borrows its parent [`group::Producer`] exclusively so
//! the borrow checker enforces that only one frame is open at a time. A [`Consumer`]
//! reads one frame, sharing the group's channel rather than a per-frame one.
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::task::{Poll, ready};

use arrayvec::ArrayVec;
use bytes::{Buf, Bytes, BytesMut};

use crate::cache;
use crate::group::{self, GroupState};
use crate::time::Instant;
use crate::{Error, IntoBytes, Result, Timestamp, stats};

/// A chunk of data with an upfront size and an optional presentation timestamp.
///
/// This is just the header; the payload is carried separately (as a completed
/// [`Frame`] or streamed via [`Producer`] / [`Consumer`]).
#[derive(Clone, Copy, Debug)]
pub struct Info {
	/// Total payload size in bytes. Declared up front so consumers can preallocate.
	pub size: u64,
	/// Presentation timestamp, or `None` on an untimed track. It must match the track
	/// ([`Error::TimestampMismatch`] otherwise).
	///
	/// [`group::Producer::create_frame`] converts it into the parent track's
	/// timescale, so the scale you build it with doesn't have to match the track.
	/// No receiver fills one in, so an untimed track stays untimed across hops, except
	/// over moq-lite 05 and 06, which can't mark a track untimed and carry the encoder's
	/// send time instead.
	pub timestamp: Option<Timestamp>,
}

/// A completed frame: an optional timestamp, its full, contiguous payload, and when it
/// arrived.
///
/// Reading a frame whole is a zero-copy slice of what its group stores, except for a
/// frame larger than a storage segment that arrived in pieces: the group keeps that as
/// separate segments, so a whole read copies them into one buffer. Streaming it through
/// [`Consumer::poll_read_chunk`] hands out each segment without a copy.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp, at the parent track's timescale, or `None` when untimed.
	pub timestamp: Option<Timestamp>,
	/// The full frame payload.
	pub payload: Bytes,
	// Stamped as the frame enters a group's cache; see [`Self::arrived`].
	pub(super) arrived: Instant,
}

impl Frame {
	/// A frame of `payload` presented at `timestamp`, or `None` on an untimed track.
	pub fn new(timestamp: impl Into<Option<Timestamp>>, payload: impl IntoBytes) -> Self {
		Self {
			timestamp: timestamp.into(),
			payload: payload.into_bytes(),
			// Stamped when written to a group, so building one never reads the clock.
			arrived: Instant::ZERO,
		}
	}

	/// When the frame's last byte landed in this node's cache, on the ambient clock.
	///
	/// A frame read from a group carries the instant it was written, received, or fetched
	/// into it, so a reader judges lateness on that rather than on when it got around to
	/// reading. A frame not yet written to a group carries the clock's origin.
	pub fn arrived(&self) -> Instant {
		self.arrived
	}
}

/// Bytes in each sealed segment of a frame larger than one.
///
/// A frame within one segment is stored as a single buffer. A larger one that arrives in
/// pieces is a chain of segments of exactly this size, the last shorter, so segment `i`
/// always holds the frame's bytes from `i * SEGMENT`. Receiving it allocates at most one
/// segment ahead of what arrived, and returns each segment's receive credit as it seals,
/// so a connection window only has to fit a segment rather than a frame.
///
/// Sized from `benches/frame.rs`: smaller segments cost allocations per byte, and larger
/// ones a bigger allocation and credit held ahead of the bytes. Most media frames fit in
/// one, so a whole read of them never copies.
pub(crate) const SEGMENT: usize = 1024 * 1024;

/// A completed frame's payload as its group stores it: one buffer, or the sealed
/// segments of a frame larger than [`SEGMENT`] written in pieces.
///
/// A view, like [`Bytes`]: reading it as a [`Buf`] moves this copy past bytes, never the
/// stored one.
#[derive(Clone, Debug)]
pub(crate) enum Payload {
	Whole(Bytes),
	Chain(Chain),
}

/// A frame's sealed segments, read from `start`.
#[derive(Clone, Debug)]
pub(crate) struct Chain {
	// Each [`SEGMENT`] long but the last, and never empty.
	segments: Arc<[Bytes]>,
	// The frame offset this view starts at.
	start: usize,
}

impl Chain {
	/// The bytes from `start` to the end.
	pub fn len(&self) -> usize {
		self.end() - self.start
	}

	/// The whole chain copied into one buffer.
	#[cold]
	#[inline(never)]
	pub fn concat(self) -> Bytes {
		Payload::Chain(self).contiguous(0)
	}

	/// The frame's length, past which nothing is stored.
	fn end(&self) -> usize {
		let last = self.segments.last().map_or(0, Bytes::len);
		self.segments.len().saturating_sub(1) * SEGMENT + last
	}

	/// The stored bytes from frame offset `at` to the end of its segment, zero-copy.
	fn segment_at(&self, at: usize) -> Bytes {
		let index = at / SEGMENT;
		match self.segments.get(index) {
			Some(segment) => segment.slice(at - index * SEGMENT..),
			None => Bytes::new(),
		}
	}
}

// A chain costs a cached frame nothing over the one buffer it holds otherwise.
const _: () = assert!(size_of::<Payload>() == size_of::<Bytes>());

impl Default for Payload {
	fn default() -> Self {
		Self::Whole(Bytes::new())
	}
}

impl From<Bytes> for Payload {
	fn from(bytes: Bytes) -> Self {
		Self::Whole(bytes)
	}
}

impl Payload {
	pub fn len(&self) -> usize {
		match self {
			Self::Whole(bytes) => bytes.len(),
			Self::Chain(chain) => chain.len(),
		}
	}

	#[cfg(feature = "async")]
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// The bytes from `offset` to the end of the segment holding it, zero-copy.
	pub fn chunk_at(&self, offset: usize) -> Bytes {
		match self {
			Self::Whole(bytes) => bytes.slice(offset..),
			Self::Chain(chain) => chain.segment_at(chain.start + offset),
		}
	}

	/// The bytes from `offset` on in one buffer, copied only when they span segments.
	pub fn contiguous(&self, offset: usize) -> Bytes {
		let len = self.len();
		let first = self.chunk_at(offset);
		if offset + first.len() >= len {
			return first;
		}
		let mut out = BytesMut::with_capacity(len - offset);
		let mut at = offset;
		while at < len {
			let chunk = self.chunk_at(at);
			at += chunk.len();
			out.extend_from_slice(&chunk);
		}
		out.freeze()
	}
}

impl Buf for Payload {
	fn remaining(&self) -> usize {
		self.len()
	}

	fn chunk(&self) -> &[u8] {
		match self {
			Self::Whole(bytes) => bytes,
			Self::Chain(chain) => {
				let index = chain.start / SEGMENT;
				chain
					.segments
					.get(index)
					.map_or(&[], |segment| &segment[chain.start - index * SEGMENT..])
			}
		}
	}

	fn advance(&mut self, cnt: usize) {
		assert!(cnt <= self.len(), "advance past the end of a payload");
		match self {
			Self::Whole(bytes) => bytes.advance(cnt),
			Self::Chain(chain) => chain.start += cnt,
		}
	}

	// Zero-copy within a segment, which is what a transport taking `chunk()` asks for.
	fn copy_to_bytes(&mut self, len: usize) -> Bytes {
		let chunk = self.chunk_at(0);
		let out = match len <= chunk.len() {
			true => chunk.slice(..len),
			false => self.contiguous(0).slice(..len),
		};
		self.advance(len);
		out
	}
}

/// A completed frame as its group stores it.
#[derive(Clone, Debug)]
pub(crate) struct Cached {
	pub timestamp: Option<Timestamp>,
	pub payload: Payload,
	// When the frame's last byte landed in this node's cache; see [`Frame::arrived`].
	pub arrived: Instant,
}

impl From<Frame> for Cached {
	fn from(frame: Frame) -> Self {
		Self {
			timestamp: frame.timestamp,
			payload: Payload::Whole(frame.payload),
			arrived: frame.arrived,
		}
	}
}

/// A frame with the relay metadata its group keeps beside it, read only by wire egress.
#[cfg(feature = "async")]
#[derive(Clone, Debug)]
pub(crate) struct Object {
	pub frame: Cached,
	pub fields: Option<Arc<super::object::Fields>>,
	/// The inflated size of a compressed frame read in its stored form (see
	/// [`group::Consumer::compressed`]), whose `payload` is then the DEFLATE slice.
	pub inflated: Option<u64>,
}

#[cfg(feature = "async")]
impl std::ops::Deref for Object {
	type Target = Cached;
	fn deref(&self) -> &Self::Target {
		&self.frame
	}
}

/// A streamed frame's private header, alongside the unchanged public header.
pub(crate) struct Header {
	pub info: Info,
	pub fields: Option<Arc<super::object::Fields>>,
	/// A received compressed frame's inflated size: `info.size` is then its DEFLATE
	/// slice's. `None` for a plain frame, or one a local producer compresses itself.
	pub inflated: Option<u64>,
}

impl From<Info> for Header {
	fn from(info: Info) -> Self {
		Self {
			info,
			fields: None,
			inflated: None,
		}
	}
}

/// A reusable batch of frames, filled by [`group::Consumer::poll_read_frames`] and drained
/// by [`group::Producer::write_frames`].
///
/// A fixed-capacity inline buffer: `N` frames of stack storage, never a heap
/// allocation and never a spill. Allocate one per task and reuse it for the life of
/// the group rather than one per read.
///
/// `N` defaults to 8. Most reads are not big batches: at the live edge a frame arrives
/// at a time, so the capacity past the first frame or two is idle stack, and a
/// publisher holds one buffer per in-flight group. 8 costs 448 bytes and still reads
/// ~5x faster than a frame at a time (`benches/group.rs`). Ask for a larger `N` when
/// you know you are draining a backlog: 32 is ~8x, for 1.75 KB.
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
	pub(crate) fn is_full(&self) -> bool {
		self.0.is_full()
	}

	/// The frames from the most recent fill, in order.
	pub fn filled(&self) -> &[Frame] {
		&self.0
	}

	/// The frames from the most recent fill, mutably, for the batch a read hands back.
	#[cfg(feature = "async")]
	pub(crate) fn filled_mut(&mut self) -> &mut [Frame] {
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

/// Bytes the frames still being written through one origin handle may allocate up front.
///
/// A writer declares each frame's size before sending it, and allocating that size up
/// front saves a reallocation and copy per doubling as the payload arrives. A peer's
/// declaration costs it nothing, though, so without a bound it could commit a
/// [`SEGMENT`] per stream for a single byte. A frame reserves what one segment of it
/// takes, and while that fits what remains each segment is allocated whole; past that,
/// it grows with the bytes received. Each frame returns its share once it completes or
/// aborts.
///
/// Every broadcast, track, and group minted from an [`crate::origin::Producer`] handle shares
/// its budget. The built-in sessions each ingest through a handle with its own.
#[derive(Clone, Default)]
pub(crate) struct Budget(Arc<Remaining>);

struct Remaining(AtomicUsize);

impl Default for Remaining {
	fn default() -> Self {
		Self(AtomicUsize::new(Budget::DEFAULT))
	}
}

impl std::fmt::Debug for Budget {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("Budget")
			.field(&self.0.0.load(Ordering::Relaxed))
			.finish()
	}
}

impl Budget {
	/// Enough for a handful of large keyframes in flight at once.
	const DEFAULT: usize = 16 * 1024 * 1024;

	/// A budget of `bytes`, for a benchmark sweeping it.
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn new(bytes: usize) -> Self {
		Self(Arc::new(Remaining(AtomicUsize::new(bytes))))
	}

	/// Take `size` bytes of the budget, or `None` if that is more than remains.
	pub(super) fn reserve(&self, size: usize) -> Option<Reservation> {
		self.0
			.0
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(size))
			.ok()?;
		Some(Reservation {
			budget: self.clone(),
			size,
		})
	}
}

/// A frame's share of a [`Budget`], returned on drop.
pub(super) struct Reservation {
	budget: Budget,
	size: usize,
}

impl Drop for Reservation {
	fn drop(&mut self) {
		self.budget.0.0.fetch_add(self.size, Ordering::Relaxed);
	}
}

/// One delivery's progress over a frame's canonical payload storage.
///
/// A whole-frame [`Bytes`] write is stored directly. Chunked writes are copied into a
/// heap [`Segment`], or a chain of them for a frame larger than [`SEGMENT`], each either
/// allocated at its full length by its first byte or grown as bytes arrive (see
/// [`Budget`]). Clones share delivery progress; [`Self::fork`] starts another delivery
/// over the same content. Publication serializes their writes, never rewriting bytes
/// already visible to readers, while each delivery advances only as its bytes arrive.
///
/// The delivery that creates the content keeps its progress in the content itself, so
/// a frame with one delivery, nearly every frame, costs one allocation. Only a fork
/// allocates a [`View`] of its own.
#[derive(Clone)]
pub(super) struct FrameBuf {
	content: Arc<FrameContent>,
	fork: Option<Arc<View>>,
}

/// A forked delivery's progress, shared by its clones.
#[derive(Default)]
struct View {
	received: AtomicUsize,
	bill: OnceLock<cache::Extra>,
}

struct FrameContent {
	size: usize,
	grow: AtomicBool,
	written: AtomicUsize,
	// The creating delivery's received bytes; a fork counts its own in its [`View`].
	received: AtomicUsize,
	storage: OnceLock<FrameStorage>,
	// Copied storage frozen once, so every delivery shares one payload. Whole storage
	// freezes as itself and leaves this empty.
	frozen: OnceLock<Payload>,
	publication: Mutex<Publication>,
	bill: OnceLock<cache::Extra>,
	identity: OnceLock<Identity>,
}

struct Identity {
	owner: Arc<group::Content>,
	key: group::ContentKey,
}

impl Drop for FrameContent {
	fn drop(&mut self) {
		if let Some(identity) = self.identity.get() {
			identity.owner.forget_frame(
				identity.key,
				self as *const Self as usize,
				self.bill.get().map_or(0, cache::Extra::size),
			);
		}
	}
}

#[derive(Clone)]
pub(super) struct Shared(Arc<FrameContent>);

#[derive(Clone)]
pub(super) struct SharedWeak(Weak<FrameContent>);

impl SharedWeak {
	pub(super) fn upgrade(&self) -> Option<Shared> {
		self.0.upgrade().map(Shared)
	}
	pub(super) fn address(&self) -> usize {
		self.0.as_ptr() as usize
	}
}

impl Shared {
	pub(super) fn from_payload(payload: Payload) -> Self {
		let size = payload.len();
		Self(Arc::new(FrameContent {
			size,
			grow: AtomicBool::new(false),
			written: AtomicUsize::new(size),
			received: AtomicUsize::new(size),
			storage: OnceLock::from(FrameStorage::Shared(payload)),
			frozen: OnceLock::new(),
			publication: Mutex::default(),
			bill: OnceLock::new(),
			identity: OnceLock::new(),
		}))
	}
	pub(super) fn downgrade(&self) -> SharedWeak {
		SharedWeak(Arc::downgrade(&self.0))
	}
	pub(super) fn fork(&self) -> FrameBuf {
		FrameBuf {
			content: self.0.clone(),
			fork: Some(Arc::default()),
		}
	}
	pub(super) fn freeze(&self) -> Payload {
		self.0.freeze()
	}
	pub(super) fn size(&self) -> usize {
		self.0.size
	}
	#[cfg(test)]
	pub(super) fn written(&self) -> usize {
		self.0.written.load(Ordering::Acquire)
	}
	pub(super) fn bill_content(&self, bill: cache::Extra) {
		self.0.bill_content(bill);
	}
	#[cfg(test)]
	pub(super) fn content_size(&self) -> u64 {
		self.0.bill.get().map_or(0, cache::Extra::size)
	}
	pub(super) fn identify(&self, owner: Arc<group::Content>, key: group::ContentKey) -> bool {
		let _publication = self.0.publication.lock().unwrap_or_else(PoisonError::into_inner);
		if self
			.0
			.identity
			.set(Identity {
				owner: owner.clone(),
				key,
			})
			.is_err()
		{
			return false;
		}
		owner.charge_frame(self.0.bill.get().map_or(0, cache::Extra::size));
		true
	}
}

impl FrameContent {
	fn bill_content(&self, bill: cache::Extra) {
		let content = self;
		let _publication = content.publication.lock().unwrap_or_else(PoisonError::into_inner);
		content.bill.get_or_init(|| {
			let written = self.written.load(Ordering::Relaxed) as u64;
			// A lazy owner may take already-billed payload from the original delivery.
			let fresh = written
				.checked_sub(bill.size())
				.expect("transferred payload cannot exceed published bytes");
			bill.add(fresh + FrameBuf::CONTENT_HEAP as u64);
			if let Some(identity) = self.identity.get() {
				identity.owner.charge_frame(bill.size());
			}
			bill
		});
	}

	/// Caller holds publication so attachment and growth update one aggregate.
	fn add_charge(&self, bytes: u64) {
		if let Some(bill) = self.bill.get() {
			let before = bill.size();
			bill.add(bytes);
			if let Some(identity) = self.identity.get() {
				identity.owner.charge_frame(bill.size() - before);
			}
		}
	}

	fn freeze(&self) -> Payload {
		debug_assert_eq!(self.written.load(Ordering::Acquire), self.size);
		// A whole payload is already its own frozen form; only copied storage builds one.
		if let Some(FrameStorage::Shared(payload)) = self.storage.get() {
			return payload.clone();
		}
		self.frozen
			.get_or_init(|| match self.storage.get() {
				Some(FrameStorage::Chain(segments)) => {
					let segments = segments.lock().unwrap_or_else(PoisonError::into_inner);
					debug_assert!(segments.open.is_none(), "froze a frame mid-segment");
					Payload::Chain(Chain {
						segments: segments.sealed.as_slice().into(),
						start: 0,
					})
				}
				_ => Payload::Whole(self.slice(0, self.size)),
			})
			.clone()
	}

	fn slice(&self, start: usize, end: usize) -> Bytes {
		if start >= end {
			return Bytes::new();
		}
		let (segment, base) = match self.storage.get() {
			Some(FrameStorage::Shared(payload)) => {
				let chunk = payload.chunk_at(start);
				return chunk.slice(..chunk.len().min(end - start));
			}
			Some(FrameStorage::Fixed(segment)) => (segment.clone(), 0),
			Some(FrameStorage::Growing(current)) => (current.lock().unwrap_or_else(PoisonError::into_inner).clone(), 0),
			Some(FrameStorage::Chain(segments)) => {
				let segments = segments.lock().unwrap_or_else(PoisonError::into_inner);
				let index = start / SEGMENT;
				let base = index * SEGMENT;
				if let Some(sealed) = segments.sealed.get(index) {
					return sealed.slice(start - base..end.min(base + SEGMENT) - base);
				}
				let open = segments.open.clone().expect("written bytes are sealed or open");
				(open, base)
			}
			// Nothing written, so the range is empty.
			None => return Bytes::new(),
		};
		let len = end.min(base + SEGMENT) - base;
		debug_assert!(len <= segment.capacity);
		Bytes::from_owner(Filled { segment, len }).slice(start - base..)
	}
}

/// What a frame's concurrent deliveries share while publishing into its content.
#[derive(Default)]
struct Publication {
	// Producers holding the frame's admission; the last one to end releases it.
	producers: usize,
	// The admission's share of the budget, if it got one. Even a failed reservation is
	// shared, so overlapping deliveries use the same growth strategy without each
	// attempting another admission.
	reserved: Option<Reservation>,
}

/// One producer's hold on its canonical frame's admission. The reservation returns to
/// the budget once no producer of the frame remains active.
///
/// The admission lives in the frame's [`Publication`] rather than an allocation of its
/// own: every streamed frame takes one, and nearly all have a single producer.
pub(super) struct Admission(Arc<FrameContent>);

impl Admission {
	/// Whether the frame holds a share of the budget.
	#[cfg(test)]
	fn reserved(&self) -> bool {
		self.0
			.publication
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.reserved
			.is_some()
	}
}

impl Drop for Admission {
	fn drop(&mut self) {
		let mut publication = self.0.publication.lock().unwrap_or_else(PoisonError::into_inner);
		publication.producers -= 1;
		if publication.producers == 0 {
			publication.reserved = None;
		}
	}
}

enum FrameStorage {
	Shared(Payload),
	Fixed(Arc<Segment>),
	// Replaced by a larger copy whenever a write outgrows it. A reader clones the current
	// segment under the lock, so a slice it already handed out keeps the old one alive.
	Growing(Mutex<Arc<Segment>>),
	// A frame larger than one segment.
	Chain(Mutex<Segments>),
}

/// A frame larger than one [`SEGMENT`], as it streams in.
#[derive(Default)]
struct Segments {
	// Every full segment so far, in order.
	sealed: Vec<Bytes>,
	// The segment being written, allocated by its first byte and grown like
	// [`FrameStorage::Growing`].
	open: Option<Arc<Segment>>,
}

impl Segments {
	/// Copy `src` in at frame offset `offset`, sealing each segment it fills.
	fn append(&mut self, mut offset: usize, mut src: &[u8], size: usize, grow: bool) {
		while !src.is_empty() {
			let base = offset - offset % SEGMENT;
			let len = (size - base).min(SEGMENT);
			let within = offset - base;
			let take = src.len().min(len - within);
			let open = self
				.open
				.get_or_insert_with(|| Arc::new(Segment::new(if grow { 0 } else { len })));
			Segment::reserve(open, within, within + take, len);
			// Safety: sole writer, within the segment's length, past every byte handed out.
			unsafe { open.write(within, &src[..take]) };
			if within + take == len {
				let segment = self.open.take().expect("written above");
				self.sealed.push(Bytes::from_owner(Filled { segment, len }));
			}
			offset += take;
			src = &src[take..];
		}
	}
}

/// An owned, zero-initialized heap allocation that chunked writes are copied into.
struct Segment {
	data: *mut u8,
	capacity: usize,
}

// Safety: `data` is owned (Box-allocated, freed in Drop). Publication serializes
// writers, which never rewrite published bytes; consumers only read `< written`.
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

	/// Replace `current` with a larger copy of its first `filled` bytes if it holds fewer
	/// than `needed` of the `len` it grows to.
	///
	/// Doubling keeps the copies linear in `len`, and the cap makes the last copy exactly
	/// `len`, so it seals without another.
	fn reserve(current: &mut Arc<Segment>, filled: usize, needed: usize, len: usize) {
		if current.capacity >= needed {
			return;
		}
		let next = Segment::new(needed.max(current.capacity * 2).min(len));
		let filled = Filled {
			segment: current.clone(),
			len: filled,
		};
		// Safety: `next` is new, so nothing reads it yet.
		unsafe { next.write(0, filled.as_ref()) };
		*current = Arc::new(next);
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
	pub(super) const CONTENT_HEAP: usize = 2 * size_of::<usize>() + size_of::<FrameContent>();
	pub(super) const VIEW_HEAP: usize = 2 * size_of::<usize>() + size_of::<View>();

	/// A buffer for a frame of `size` bytes, allocated at that size on the first write.
	///
	/// The oversized-frame guard lives in [`group::Producer`], which rejects a declared
	/// size larger than the group's byte budget before calling this.
	pub(super) fn new(size: usize) -> Self {
		Self::with_growth(size, false)
	}

	/// A buffer for a frame of `size` bytes that allocates only as bytes arrive.
	#[cfg(test)]
	pub(super) fn growing(size: usize) -> Self {
		Self::with_growth(size, true)
	}

	fn with_growth(size: usize, grow: bool) -> Self {
		Self {
			content: Arc::new(FrameContent {
				size,
				grow: AtomicBool::new(grow),
				written: AtomicUsize::new(0),
				received: AtomicUsize::new(0),
				storage: OnceLock::new(),
				frozen: OnceLock::new(),
				publication: Mutex::default(),
				bill: OnceLock::new(),
				identity: OnceLock::new(),
			}),
			fork: None,
		}
	}

	/// Keep an already-complete payload without copying its buffers or segments.
	#[cfg(test)]
	pub(super) fn from_payload(payload: Payload) -> Self {
		Self {
			content: Shared::from_payload(payload).0,
			fork: None,
		}
	}

	pub(super) fn shared(&self) -> Shared {
		Shared(self.content.clone())
	}

	/// Another delivery of the same immutable frame, starting before its first byte.
	#[cfg(test)]
	pub(super) fn fork(&self) -> Self {
		self.shared().fork()
	}

	/// Bill physical content once, including bytes already received before attachment.
	/// The charge follows its buffers even if a cached row lets go before a stream does.
	#[cfg(test)]
	pub(super) fn bill_content(&self, bill: cache::Extra) {
		self.content.bill_content(bill);
	}

	/// One delivery's metadata, retained by its streaming readers and shared by clones.
	/// The creating delivery's lives in its content, so only a fork's is billed here.
	pub(super) fn bill_view(&self, bill: cache::Extra) {
		if let Some(view) = &self.fork {
			view.bill.get_or_init(|| {
				bill.add(Self::VIEW_HEAP as u64);
				bill
			});
		}
	}

	/// The canonical allocation's charge, counted once by its cache owner.
	#[cfg(test)]
	pub(super) fn content_size(&self) -> u64 {
		self.content.bill.get().map_or(0, cache::Extra::size)
	}

	/// This delivery's charge; clones retain the same view allocation.
	pub(super) fn view_size(&self) -> u64 {
		self.fork
			.as_ref()
			.and_then(|view| view.bill.get())
			.map_or(0, cache::Extra::size)
	}

	/// This delivery's received bytes.
	fn received(&self) -> &AtomicUsize {
		match &self.fork {
			Some(view) => &view.received,
			None => &self.content.received,
		}
	}

	/// Share the existing per-frame admission until its last active producer ends.
	/// Complete canonical data is replayed without reserving memory again.
	pub(super) fn admit(&self, budget: &Budget) -> Option<Admission> {
		let content = &self.content;
		let mut publication = content.publication.lock().unwrap_or_else(PoisonError::into_inner);
		if self.shared_written(Ordering::Acquire) == self.size() {
			return None;
		}
		if publication.producers == 0 {
			let reserved = budget.reserve(self.size().min(SEGMENT));
			// Existing bytes keep their allocations. A later admission may only make
			// future growth more conservative once storage already exists.
			if reserved.is_none() || content.storage.get().is_none() {
				content.grow.store(reserved.is_none(), Ordering::Relaxed);
			}
			publication.reserved = reserved;
		}
		publication.producers += 1;
		drop(publication);
		Some(Admission(content.clone()))
	}

	/// The frame's declared size.
	pub(super) fn size(&self) -> usize {
		self.content.size
	}

	/// Bytes this delivery received, independently of other copies' progress.
	pub(super) fn written(&self, ord: Ordering) -> usize {
		self.received().load(ord)
	}

	/// Unique canonical bytes published so far, for shared-content accounting.
	pub(super) fn shared_written(&self, ord: Ordering) -> usize {
		self.content.written.load(ord)
	}

	fn try_set_bytes(&self, bytes: Bytes) -> std::result::Result<(), Bytes> {
		if bytes.len() != self.size() || self.shared_written(Ordering::Acquire) != 0 {
			return Err(bytes);
		}
		self.content
			.storage
			.set(FrameStorage::Shared(Payload::Whole(bytes)))
			.map_err(|storage| match storage {
				FrameStorage::Shared(Payload::Whole(bytes)) => bytes,
				_ => unreachable!("try_set_bytes only installs shared storage"),
			})
	}

	/// Safety: caller holds the publication mutex and `new_written` is `<= size`.
	unsafe fn store_written(&self, new_written: usize) {
		let content = &self.content;
		let before = content.written.load(Ordering::Relaxed);
		content.add_charge((new_written - before) as u64);
		// Release pairs with consumers' Acquire load to publish prior writes.
		content.written.store(new_written, Ordering::Release);
	}

	/// Validate this delivery's overlap, then publish only its unseen suffix.
	pub(super) fn append(&self, src: &[u8]) -> Result<()> {
		self.write_chunk(src)
	}

	fn write_chunk<B: IntoBytes>(&self, chunk: B) -> Result<()> {
		let content = &self.content;
		let _publication = content.publication.lock().unwrap_or_else(PoisonError::into_inner);
		let start = self.written(Ordering::Relaxed);
		let end = start
			.checked_add(chunk.as_ref().len())
			.filter(|end| *end <= self.size())
			.ok_or(Error::WrongSize)?;
		let published = self.shared_written(Ordering::Acquire);
		if start > published {
			return Err(Error::ProtocolViolation);
		}
		let overlap = end.min(published);
		let mut at = start;
		while at < overlap {
			let prior = self.slice(at, overlap);
			if prior.as_ref() != &chunk.as_ref()[at - start..at - start + prior.len()] {
				return Err(Error::ProtocolViolation);
			}
			at += prior.len();
		}
		if end > published {
			if published == 0 && end == self.size() {
				match self.try_set_bytes(chunk.into_bytes()) {
					Ok(()) => {
						// Safety: the mutex excludes other publishers; the full size was checked.
						unsafe { self.store_written(end) };
					}
					Err(chunk) => self.append_suffix(&chunk),
				}
			} else {
				self.append_suffix(&chunk.as_ref()[published - start..]);
			}
		}
		self.received().store(end, Ordering::Release);
		Ok(())
	}

	/// Append after the shared prefix while holding the publication mutex.
	fn append_suffix(&self, src: &[u8]) {
		if src.is_empty() {
			return;
		}
		let prev = self.shared_written(Ordering::Relaxed);
		let size = self.size();
		let content = &self.content;
		let grow = content.grow.load(Ordering::Relaxed);
		let storage = content.storage.get_or_init(|| match grow {
			_ if size > SEGMENT => FrameStorage::Chain(Mutex::default()),
			true => FrameStorage::Growing(Mutex::new(Arc::new(Segment::new(0)))),
			false => FrameStorage::Fixed(Arc::new(Segment::new(size))),
		});
		// Safety (every write below): the publication mutex makes this the sole writer;
		// the caller checked the remaining size, and consumers only read `[..written]`.
		match storage {
			// Only reachable if the frame is already complete via shared storage, which
			// `Producer::write` rejects for a non-empty chunk. Nothing to copy.
			FrameStorage::Shared(_) => return,
			FrameStorage::Fixed(segment) => unsafe { segment.write(prev, src) },
			FrameStorage::Growing(current) => {
				let mut current = current.lock().unwrap_or_else(PoisonError::into_inner);
				Segment::reserve(&mut current, prev, prev + src.len(), size);
				unsafe { current.write(prev, src) };
			}
			FrameStorage::Chain(segments) => {
				let mut segments = segments.lock().unwrap_or_else(PoisonError::into_inner);
				segments.append(prev, src, size, grow);
			}
		}
		// Safety: the publication mutex excludes other writers, and the caller checked the size.
		unsafe { self.store_written(prev + src.len()) };
	}

	/// The completed payload, shared by every delivery without copying its segments.
	/// Called only once the canonical payload is complete. Delivery completion is
	/// checked separately by [`Producer::finish`].
	pub(super) fn freeze(&self) -> Payload {
		self.content.freeze()
	}

	/// A zero-copy slice of the written region from `start` to `end`, or to the end of
	/// `start`'s segment if that comes first.
	///
	/// `end` must not exceed a shared published value already loaded with `Acquire`.
	/// A delivery's own received count is always within that prefix too.
	fn slice(&self, start: usize, end: usize) -> Bytes {
		self.content.slice(start, end)
	}

	/// Heap bytes the buffer currently holds for the payload.
	#[cfg(test)]
	pub(super) fn allocated(&self) -> usize {
		match self.content.storage.get() {
			Some(FrameStorage::Shared(payload)) => payload.len(),
			Some(FrameStorage::Fixed(segment)) => segment.capacity,
			Some(FrameStorage::Growing(current)) => current.lock().unwrap_or_else(PoisonError::into_inner).capacity,
			Some(FrameStorage::Chain(segments)) => {
				let segments = segments.lock().unwrap_or_else(PoisonError::into_inner);
				let sealed: usize = segments.sealed.iter().map(Bytes::len).sum();
				sealed + segments.open.as_ref().map_or(0, |open| open.capacity)
			}
			None => 0,
		}
	}
}

/// Writes the payload of a group's single in-flight frame in one or more chunks.
///
/// Created by [`group::Producer::create_frame`], which hands it the group's writer: no
/// other frame can be opened, and the group cannot finish, until [`Self::finish`] returns
/// the writer. Owned rather than borrowed, so a driver can hold it across polls while
/// the payload streams in. [`Self::group`] still reads the group meanwhile.
///
/// The total bytes written must exactly match [`Info::size`]; call [`Self::finish`] to
/// commit the frame (or [`Self::abort`] to fail it). Dropping it without either aborts
/// the group, since an unfinished frame leaves the group's stream broken.
///
/// A single whole-frame [`write`](Self::write) keeps the caller's allocation
/// (zero-copy). Chunked writes copy into one buffer, or a chain of fixed-size segments
/// for a large frame, each allocated whole by its first byte, or grown as bytes arrive
/// once the frames in flight pass the origin's budget. Nothing is allocated past the
/// segment being written, whatever size was declared.
pub struct Producer {
	group: Option<group::Producer>,
	buf: FrameBuf,
	info: Info,
	// Set once the frame is committed (finished) or aborted, so Drop is a no-op.
	done: bool,
	// A receive handler's cancellation cleanup, carried across polls.
	cancel_on_drop: bool,
	// Ingress payload meter, inherited from the parent group. Counts each written
	// chunk's bytes. Empty (no-op) for an untagged group.
	stats: stats::Meter,
	// The frame's share of the origin's [`Budget`], if it got one, held until it ends.
	_reserved: Option<Admission>,
}

impl std::ops::Deref for Producer {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Producer {
	pub(super) fn new(group: group::Producer, buf: FrameBuf, info: Info, reserved: Option<Admission>) -> Self {
		Self {
			group: Some(group),
			buf,
			info,
			done: false,
			cancel_on_drop: false,
			stats: stats::Meter::default(),
			_reserved: reserved,
		}
	}

	/// Attach the parent group's ingress meter, so written chunks bump `bytes`.
	pub(super) fn with_meter(mut self, meter: stats::Meter) -> Self {
		self.stats = meter;
		self
	}

	/// Transfer a receive handler's cancellation cleanup into its in-flight frame.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	// A dropped receive handler cannot name its error; a public driver aborts explicitly.
	pub(crate) fn cancel_on_drop(mut self) -> Self {
		self.cancel_on_drop = true;
		self
	}

	/// The group this frame belongs to, readable while the frame holds its writer.
	pub fn group(&self) -> &group::Producer {
		self.group.as_ref().expect("frame owns its group")
	}

	/// Bytes still needed to complete the frame.
	pub fn remaining(&self) -> usize {
		self.buf.size() - self.buf.written(Ordering::Acquire)
	}

	/// Bytes still needed to seal the segment being written, which for a frame within one
	/// segment is the rest of the frame.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn unsealed(&self) -> usize {
		let written = self.buf.written(Ordering::Acquire);
		let end = (written / SEGMENT + 1).saturating_mul(SEGMENT).min(self.buf.size());
		end - written
	}

	/// Write a chunk of data to the frame, waking its readers.
	///
	/// Returns [`Error::WrongSize`] if the chunk would exceed the remaining bytes.
	pub fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		self.write_deferred(chunk)?;
		self.notify();
		Ok(())
	}

	/// Write a chunk of payload *without* waking consumers; pair it with [`Self::notify`].
	///
	/// The wake is split out because the wire ingest drains every chunk the transport
	/// has already buffered in one poll turn, and a consumer parked on the group cannot
	/// run until that turn yields. Waking per chunk pays a group lock and a clock read
	/// to publish bytes nobody can observe yet.
	///
	/// `coding::Reader::poll_read_frame` owns the pairing and is the only caller.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn write_deferred<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		let len = chunk.as_ref().len();
		if len > self.remaining() {
			return Err(Error::WrongSize);
		}
		// Ingress payload: count the chunk's bytes as they're written.
		self.stats.bytes(len as u64);
		if len == self.buf.size() && self.buf.written(Ordering::Acquire) == 0 {
			self.buf.write_chunk(chunk)
		} else {
			self.buf.append(chunk.as_ref())
		}
	}

	/// Publish what has been written so far, waking consumers parked on the group.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn notify(&self) {
		self.group().frame_notify();
	}

	/// Commit the frame, verifying that all bytes were written, and return the group's
	/// writer for its next frame or its finish.
	///
	/// Returns [`Error::WrongSize`] if the bytes written don't match [`Info::size`].
	pub fn finish(mut self) -> Result<group::Producer> {
		if self.buf.written(Ordering::Acquire) != self.buf.size() {
			return Err(Error::WrongSize);
		}
		let payload = self.buf.freeze();
		let mut group = self.group.take().expect("frame owns its group");
		let committed = group.frame_commit(Cached {
			timestamp: self.info.timestamp,
			payload,
			// Stamped by the commit, once readable.
			arrived: Instant::ZERO,
		});
		self.done = true;
		committed?;
		Ok(group)
	}

	/// Abort the frame (and its group) with the given error.
	pub fn abort(mut self, err: Error) -> Result<()> {
		self.group.as_mut().expect("frame owns its group").frame_abort(err);
		self.done = true;
		Ok(())
	}

	/// Heap bytes the frame's buffer currently holds for the payload.
	#[cfg(test)]
	pub(super) fn allocated(&self) -> usize {
		self.buf.allocated()
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		if self.done {
			return;
		}
		// An unfinished frame leaves the group stream broken; fail the group so
		// consumers surface an error instead of hanging on the partial forever.
		// A group already aborted (superseded, evicted, cancelled) carries its own
		// reason, so cutting its in-flight frame short is expected.
		let Some(group) = self.group.as_mut() else {
			return;
		};
		if !group.is_aborted() && !self.cancel_on_drop {
			tracing::warn!(
				group = group.sequence,
				"frame::Producer dropped before writing all bytes"
			);
		}
		group.frame_abort(if self.cancel_on_drop {
			Error::Cancel
		} else {
			Error::Dropped
		});
	}
}

/// The source of a [`Consumer`]'s payload: a finished frame (whole) or the in-flight
/// tail (streamed).
#[derive(Clone)]
pub(super) enum Source {
	Complete(Payload),
	Partial(FrameBuf),
}

/// Subscriber expiry state carried across the group-to-frame handoff.
#[derive(Clone)]
pub(super) struct Expiry {
	policy: Arc<dyn group::Expiry>,
	stale_stats: stats::Meter,
	stale_counted: Arc<AtomicBool>,
	tail: Range<usize>,
	count_payload: bool,
}

impl Expiry {
	pub(super) fn new(
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

	pub(super) fn for_frame(mut self, tail: Range<usize>, count_payload: bool) -> Self {
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
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fields: Option<Arc<super::object::Fields>>,
	/// The inflated size of a compressed frame read in its stored form (see
	/// [`group::Consumer::compressed`]), whose payload is then the DEFLATE slice.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) inflated: Option<u64>,
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
	pub(super) fn new(state: kio::Consumer<GroupState>, header: impl Into<Header>, source: Source) -> Self {
		let header = header.into();
		Self {
			state,
			info: header.info,
			fields: header.fields,
			inflated: header.inflated,
			source,
			read_idx: 0,
			stats: stats::Meter::default(),
			expiry: None,
			expired: false,
			recover: None,
		}
	}

	/// Carry this frame across a front's route changes; see [`super::resume`].
	pub(super) fn with_recover(mut self, recover: super::resume::Recover, index: u64) -> Self {
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
			// Read to its end: no route is needed for it any more.
			if matches!(res, Poll::Ready(Ok(_))) && self.read_idx >= self.size() {
				self.recover = None;
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
			if !recover.wants(failed.as_ref(), waiter) {
				return res;
			}
			if self.state.read().sparse() {
				return Poll::Ready(Err(Error::Unsupported));
			}
			let mut replacement = match recover.poll(*index, failed.as_ref(), waiter) {
				Poll::Ready(Ok(group)) => group,
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => return Poll::Pending,
			};
			// A cached group can precede this frame's header. Keep the old generation
			// until the replacement frame is actually available.
			if let Some(expiry) = &mut self.expiry {
				expiry.policy = expiry.policy.for_track(&replacement.copy);
			}
			// A stored compressed frame continues from the replacement's stored form, where
			// the byte offset means the same thing.
			if self.inflated.is_some() {
				replacement.group = replacement.group.compressed();
			}
			let frame = match replacement.group.poll_next_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => frame,
				Poll::Ready(Ok(None)) => return Poll::Ready(Err(Error::WrongSize)),
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => {
					return match self.poll_expired(waiter) {
						true => Poll::Ready(Err(Error::Old)),
						false => Poll::Pending,
					};
				}
			};
			// This copy was dense through its frame, so the replacement's must be the same
			// Object ID: a copy that begins past a gap holds different content there.
			if frame.fields.as_ref().is_some_and(|fields| fields.id != *index) {
				return Poll::Ready(Err(Error::Unsupported));
			}
			// Same name, same content: a different size is the routes disagreeing.
			if frame.info.size != self.info.size || frame.inflated != self.inflated {
				return Poll::Ready(Err(Error::ProtocolViolation));
			}
			recover.adopt(&replacement);
			self.state = frame.state;
			self.source = frame.source;
		}
	}

	/// Attach an egress meter so read-out chunks bump `bytes`. Used only for frames
	/// read directly from the group (whose bytes weren't counted at a batch fill).
	pub(super) fn with_meter(mut self, meter: stats::Meter) -> Self {
		self.stats = meter;
		self
	}

	pub(super) fn with_expiry(mut self, expiry: Expiry) -> Self {
		self.expiry = Some(expiry);
		self
	}

	fn size(&self) -> usize {
		match &self.source {
			Source::Complete(payload) => payload.len(),
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
		let budget = self.recover.as_ref().and_then(|recover| recover.0.poll_budget(waiter));
		if !expiry.policy.is_expired(budget, waiter) {
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
			Source::Complete(payload) => {
				if self.read_idx >= payload.len() {
					return Poll::Ready(Ok(None));
				}
				let out = payload.chunk_at(self.read_idx);
				self.read_idx += out.len();
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
				self.read_idx += out.len();
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
	#[cfg(feature = "async")]
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_read_chunk(waiter)).await
	}

	#[cfg(feature = "async")]
	fn poll_read_all_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes>> {
		if self.expired {
			return Poll::Ready(Err(Error::Old));
		}
		let buf = match &self.source {
			Source::Complete(payload) => {
				let out = payload.contiguous(self.read_idx);
				self.read_idx = payload.len();
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
		let out = buf.freeze().contiguous(read_idx);
		self.read_idx = size;
		self.stats.bytes(out.len() as u64);
		Poll::Ready(Ok(out))
	}

	/// Return all remaining bytes, blocking until the frame is finished.
	#[cfg(feature = "async")]
	pub async fn read_all(&mut self) -> Result<Bytes> {
		kio::wait(|waiter| {
			if self.recover.is_none() {
				return self.poll_read_all_once(waiter);
			}
			self.poll_resumed(waiter, Self::poll_read_all_once)
		})
		.await
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
	fn activating_shared_content_transfers_payload_without_recording_it_twice() {
		let pool = cache::Pool::unbounded();
		let account = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let mut delivery = account.charge();
		delivery.add(8);
		let before = pool.used();
		account.take_written();
		let content = FrameBuf::from_payload(Bytes::from_static(b"abcdefgh").into());
		content.bill_content(delivery.transfer(8));
		assert_eq!(content.content_size(), 8 + FrameBuf::CONTENT_HEAP as u64);
		assert_eq!(pool.used(), before + FrameBuf::CONTENT_HEAP as u64);
		assert_eq!(
			account.take_written(),
			FrameBuf::CONTENT_HEAP as u64,
			"old payload is not a new write"
		);
		delivery.add(8);
		account.take_written();
		content.bill_content(delivery.transfer(8));
		assert_eq!(
			account.take_written(),
			0,
			"an already-canonical owner records no second write"
		);
		assert_eq!(pool.used(), before + FrameBuf::CONTENT_HEAP as u64);
		delivery.clear();
		assert_eq!(pool.used(), content.content_size());
		drop(content);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn canonical_content_keeps_each_delivery_progress_independent() {
		let a = FrameBuf::growing(8);
		let b = a.fork();
		let b_reader = b.clone();
		a.append(b"abcd").unwrap();
		let early = a.slice(0, 4);
		b.append(b"ab").unwrap();
		b.append(b"cdef").unwrap();
		assert_eq!((a.written(Ordering::Acquire), b.written(Ordering::Acquire)), (4, 6));
		assert_eq!(
			b_reader.written(Ordering::Acquire),
			6,
			"clones follow their own delivery"
		);
		a.append(b"efgh").unwrap();
		assert_eq!(
			b.written(Ordering::Acquire),
			6,
			"another producer's completion credits no bytes"
		);
		assert_eq!(b.shared_written(Ordering::Acquire), 8);
		assert_eq!(b.allocated(), 8, "the overlapping payload is stored once");
		b.append(b"gh").unwrap();
		assert_eq!(early, b"abcd".as_slice());
		let (left, right) = (a.freeze().contiguous(0), b.freeze().contiguous(0));
		assert_eq!(left, b"abcdefgh".as_slice());
		assert_eq!(left.as_ptr(), right.as_ptr());
		eprintln!(
			"canonical frame metadata: content={}B, view={}B",
			FrameBuf::CONTENT_HEAP,
			FrameBuf::VIEW_HEAP
		);
	}

	/// A disagreement late in the overlap cannot publish an otherwise new suffix.
	#[test]
	fn a_conflicting_delivery_leaves_shared_content_and_progress_unchanged() {
		let size = SEGMENT + 8;
		let payload: Vec<_> = (0..size).map(|i| (i % 251) as u8).collect();
		let a = FrameBuf::growing(size);
		a.append(&payload[..SEGMENT + 4]).unwrap();
		let b = a.fork();
		b.append(&payload[..SEGMENT - 2]).unwrap();
		let allocated = a.allocated();
		let mut conflict = payload[SEGMENT - 2..].to_vec();
		conflict[3] ^= 1;
		assert!(matches!(b.append(&conflict), Err(Error::ProtocolViolation)));
		assert_eq!(b.written(Ordering::Acquire), SEGMENT - 2);
		assert_eq!(a.shared_written(Ordering::Acquire), SEGMENT + 4);
		assert_eq!(a.allocated(), allocated);
		a.append(&payload[SEGMENT + 4..]).unwrap();
		assert_eq!(a.freeze().contiguous(0), payload, "the valid delivery remains usable");
		assert!(matches!(b.append(&vec![0; size]), Err(Error::WrongSize)));
		assert_eq!(b.written(Ordering::Acquire), SEGMENT - 2);
	}

	#[test]
	fn a_complete_chain_replays_without_copying_or_readmitting_it() {
		let payload = Payload::Chain(Chain {
			segments: Arc::from([Bytes::from(vec![1; SEGMENT]), Bytes::from_static(b"tail")]),
			start: 0,
		});
		let canonical = FrameBuf::from_payload(payload.clone());
		let copy = canonical.fork();
		assert_eq!(copy.written(Ordering::Acquire), 0);
		assert!(copy.admit(&Budget::new(0)).is_none());
		let mut at = 0;
		while at < payload.len() {
			let chunk = payload.chunk_at(at);
			copy.append(&chunk).unwrap();
			at += chunk.len();
		}
		let (Payload::Chain(original), Payload::Chain(replayed)) = (&payload, copy.freeze()) else {
			panic!("the chain stays segmented");
		};
		assert!(Arc::ptr_eq(&original.segments, &replayed.segments));
		assert_eq!(copy.allocated(), payload.len());
	}

	#[test]
	fn simultaneous_deliveries_publish_one_segmented_payload() {
		const SIZE: usize = 2 * SEGMENT + 37;
		const ROUNDS: usize = 12;
		let payload: Vec<_> = (0..SIZE).map(|i| (i % 251) as u8).collect();
		for grow in [false, true] {
			let a = FrameBuf::with_growth(SIZE, grow);
			let b = a.fork();
			let barrier = std::sync::Barrier::new(2);
			std::thread::scope(|scope| {
				for (buf, chunk) in [(&a, SIZE.div_ceil(7)), (&b, SIZE.div_ceil(11))] {
					let (payload, barrier) = (&payload, &barrier);
					scope.spawn(move || {
						let mut at = 0;
						for round in 0..ROUNDS {
							barrier.wait();
							let end = ((round + 1) * chunk).min(SIZE);
							buf.append(&payload[at..end]).unwrap();
							assert_eq!(buf.written(Ordering::Acquire), end);
							at = end;
							barrier.wait();
						}
					});
				}
			});
			let (Payload::Chain(left), Payload::Chain(right)) = (a.freeze(), b.freeze()) else {
				panic!("chunked content stays segmented");
			};
			assert!(
				Arc::ptr_eq(&left.segments, &right.segments),
				"even the segment list is shared"
			);
			assert_eq!(a.allocated(), SIZE);
			assert_eq!(Payload::Chain(left).contiguous(0), payload);
		}
	}

	#[test]
	fn admission_ends_with_active_producers_not_cached_views() {
		let budget = Budget::new(SEGMENT);
		let a = FrameBuf::new(SEGMENT + 1);
		let b = a.fork();
		let a_active = a.admit(&budget).unwrap();
		let b_active = b.admit(&budget).unwrap();
		assert!(Arc::ptr_eq(&a_active.0, &b_active.0));
		assert!(
			budget.reserve(1).is_none(),
			"the second producer shares the first's reservation"
		);
		a.append(&vec![7; SEGMENT + 1]).unwrap();
		let replay = a.fork();
		assert!(
			replay.admit(&budget).is_none(),
			"complete content reserves nothing on replay"
		);
		drop(a_active);
		assert!(
			budget.reserve(1).is_none(),
			"another active delivery owns the same reservation"
		);
		drop(b_active);
		let returned = budget.reserve(SEGMENT).expect("cached views keep no admission alive");
		assert_eq!(b.written(Ordering::Acquire), 0);
		assert_eq!(replay.written(Ordering::Acquire), 0);
		drop(returned);
	}

	#[test]
	fn unadmitted_deliveries_share_the_same_growing_frame() {
		let budget = Budget::new(0);
		let a = FrameBuf::new(SEGMENT);
		let b = a.fork();
		let left = a.admit(&budget).unwrap();
		let right = b.admit(&budget).unwrap();
		assert!(Arc::ptr_eq(&left.0, &right.0));
		assert!(!left.reserved());
		a.append(b"a").unwrap();
		assert_eq!(a.allocated(), 1, "failed admission grows only with received bytes");
		b.append(b"ab").unwrap();
		assert_eq!(a.allocated(), 2);
		assert_eq!((a.written(Ordering::Acquire), b.written(Ordering::Acquire)), (1, 2));
	}

	#[test]
	fn canonical_and_delivery_charges_follow_their_last_owners() {
		let pool = cache::Pool::unbounded();
		let account = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let charge = account.charge();
		let baseline = pool.used();
		let a = FrameBuf::growing(8);
		let active = a.admit(&Budget::new(0)).unwrap();
		a.append(b"abcd").unwrap();
		assert_eq!(pool.used(), baseline, "the content has not attached its account yet");
		a.bill_content(charge.extra());
		a.bill_view(charge.extra());
		assert_eq!(
			pool.used(),
			baseline + FrameBuf::CONTENT_HEAP as u64 + 4,
			"the creating delivery's progress is billed with its content"
		);
		let b = a.fork();
		b.bill_content(charge.extra());
		b.bill_view(charge.extra());
		let reader = b.clone();
		reader.bill_view(charge.extra());
		let metadata = (FrameBuf::CONTENT_HEAP + FrameBuf::VIEW_HEAP) as u64;
		assert_eq!(
			pool.used(),
			baseline + metadata + 4,
			"late attachment and clones charge once"
		);
		b.append(b"abcdef").unwrap();
		assert_eq!(
			pool.used(),
			baseline + metadata + 6,
			"overlap contributes only two new bytes"
		);
		a.append(b"efgh").unwrap();
		assert_eq!(pool.used(), baseline + metadata + 8);
		drop(active);
		drop(a);
		assert_eq!(pool.used(), baseline + metadata + 8, "the fork still holds the content");
		drop(b);
		assert_eq!(
			pool.used(),
			baseline + metadata + 8,
			"the streaming clone owns its view and content"
		);
		drop(reader);
		assert_eq!(pool.used(), baseline, "all intrinsic charges release with content");
	}

	#[test]
	fn admission_allocates_nothing_and_completed_payload_needs_none() {
		let pool = cache::Pool::unbounded();
		let account = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let charge = account.charge();
		let baseline = pool.used();
		let a = FrameBuf::new(8);
		a.bill_content(charge.extra());
		a.bill_view(charge.extra());
		let metadata = FrameBuf::CONTENT_HEAP as u64;
		assert_eq!(pool.used(), baseline + metadata);
		let budget = Budget::new(0);
		let first = a.admit(&budget).unwrap();
		let copy = a.fork();
		let second = copy.admit(&budget).unwrap();
		assert!(Arc::ptr_eq(&first.0, &second.0));
		assert_eq!(
			pool.used(),
			baseline + metadata,
			"an admission lives in its content, allocating nothing"
		);
		drop(first);
		drop(second);
		let resumed = a.admit(&budget).unwrap();
		assert_eq!(
			pool.used(),
			baseline + metadata,
			"a renewed admission allocates nothing either"
		);
		a.append(b"abcdefgh").unwrap();
		assert_eq!(a.content_size(), (FrameBuf::CONTENT_HEAP + 8) as u64);
		assert_eq!(a.view_size(), 0, "the creating delivery's view is part of its content");
		assert_eq!(copy.view_size(), 0, "a fork starts without a delivery charge");
		drop(resumed);
		drop(a);
		drop(copy);
		assert_eq!(pool.used(), baseline);

		let payload = Bytes::from_static(b"complete");
		let complete = FrameBuf::from_payload(payload.clone().into());
		complete.bill_content(charge.extra());
		complete.bill_view(charge.extra());
		assert!(complete.admit(&budget).is_none());
		assert_eq!(complete.content_size(), (FrameBuf::CONTENT_HEAP + payload.len()) as u64);
		assert_eq!(pool.used(), baseline + metadata + payload.len() as u64);
		assert_eq!(complete.freeze().contiguous(0).as_ptr(), payload.as_ptr());
		drop(complete);
		assert_eq!(pool.used(), baseline);
	}

	/// Each reallocation leaves the slices already handed out intact, and the last
	/// segment is exactly the frame, so the payload freezes without another copy.
	#[test]
	fn growing_buffer_keeps_handed_out_slices() {
		let buf = FrameBuf::growing(10);
		buf.append(b"abc").unwrap();
		assert_eq!(buf.allocated(), 3);
		let early = buf.slice(0, 3);

		buf.append(b"defg").unwrap();
		assert_eq!(buf.allocated(), 7);
		buf.append(b"hij").unwrap();
		assert_eq!(buf.allocated(), 10, "doubling stops at the declared size");

		assert_eq!(early, &b"abc"[..]);
		assert_eq!(buf.slice(3, 7), &b"defg"[..]);
		assert_eq!(buf.freeze().contiguous(0), &b"abcdefghij"[..]);
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
			buf.append(chunk).unwrap();
		}
		reader.join().unwrap();
		assert_eq!(buf.freeze().contiguous(0), payload);
	}

	/// A frame larger than a segment allocates only the segment being written, whatever it
	/// declared, and seals each at the same fixed boundary. Reads never cross one, and a
	/// whole read copies the chain into one buffer.
	#[test]
	fn chain_allocates_a_segment_at_a_time() {
		const SIZE: usize = 2 * SEGMENT + 100;
		// Straddles every segment boundary.
		const CHUNK: usize = 300_000;
		let payload: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();

		for grow in [false, true] {
			let buf = FrameBuf::with_growth(SIZE, grow);
			let mut read = 0;
			for chunk in payload.chunks(CHUNK) {
				buf.append(chunk).unwrap();
				let written = buf.written(Ordering::Acquire);
				let ahead = written.next_multiple_of(SEGMENT).min(SIZE);
				assert!(
					buf.allocated() <= ahead,
					"grow={grow}: {} past {written}",
					buf.allocated()
				);

				while read < written {
					let slice = buf.slice(read, written);
					assert_eq!(slice, payload[read..read + slice.len()]);
					assert!(
						read / SEGMENT == (read + slice.len() - 1) / SEGMENT,
						"a read crossed a segment"
					);
					read += slice.len();
				}
			}

			let Payload::Chain(chain) = buf.freeze() else {
				panic!("grow={grow}: a frame past one segment is a chain");
			};
			let lens: Vec<usize> = chain.segments.iter().map(Bytes::len).collect();
			assert_eq!(lens, [SEGMENT, SEGMENT, 100]);
			assert_eq!(buf.freeze().contiguous(0), payload);
			assert_eq!(buf.freeze().contiguous(SEGMENT + 1), payload[SEGMENT + 1..]);
		}
	}

	/// A chain read as a [`Buf`] hands out each segment without a copy and advances only
	/// its own view.
	#[test]
	fn chain_reads_as_a_buf() {
		let first = Bytes::from(vec![1u8; SEGMENT]);
		let stored = Payload::Chain(Chain {
			segments: Arc::from([first.clone(), Bytes::from_static(b"tail")]),
			start: 0,
		});

		let mut view = stored.clone();
		assert_eq!(view.chunk().len(), SEGMENT);
		let head = view.copy_to_bytes(SEGMENT - 1);
		assert_eq!(head.as_ptr(), first.as_ptr(), "a read within a segment copied");
		assert_eq!(view.copy_to_bytes(3), &[1, b't', b'a'][..], "a read across segments");
		assert_eq!(view.chunk(), b"il");
		assert_eq!(view.remaining(), 2);
		assert_eq!(stored.len(), SEGMENT + 4, "the stored payload never moved");
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
