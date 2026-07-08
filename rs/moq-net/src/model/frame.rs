//! Frames are the leaf of the model: a sized, timestamped payload within a group.
//!
//! A group is a single ordered stream, so at most one frame is ever in flight.
//! Completed frames are plain data ([`Frame`]); the in-flight frame is written
//! through [`Producer`], which borrows its parent [`group::Producer`] exclusively so
//! the borrow checker enforces that only one frame is open at a time. A [`Consumer`]
//! reads one frame, sharing the group's channel rather than a per-frame one.
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::group::{self, GroupState};
use crate::{Error, IntoBytes, Result, Timestamp};

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
	/// Use [`group::Producer::create_frame_now`] /
	/// [`group::Producer::write_frame_now`] to stamp wall-clock time instead of
	/// supplying one explicitly.
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

/// Payload storage for the single in-flight frame, shared between the writing
/// [`Producer`] and any streaming [`Consumer`]s.
///
/// A whole-frame [`Bytes`] write is stored directly. Chunked writes fall back to one
/// mutable heap allocation sized to the declared frame. The producer writes through
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

struct MutableFrameBuf {
	// Owned heap allocation of `capacity` bytes (zero-initialized).
	data: *mut u8,
	capacity: usize,
}

// Safety: `data` is owned (Box-allocated, freed in Drop). The producer is the sole
// writer and consumers only read bytes `< written`.
unsafe impl Send for MutableFrameBuf {}
unsafe impl Sync for MutableFrameBuf {}

impl Drop for MutableFrameBuf {
	fn drop(&mut self) {
		// Safety: data was obtained from `Box::into_raw` of a `Box<[u8]>` of length
		// `capacity` and is not aliased at drop (Arc refcount hit 0).
		unsafe {
			let slice = std::ptr::slice_from_raw_parts_mut(self.data, self.capacity);
			drop(Box::from_raw(slice));
		}
	}
}

impl MutableFrameBuf {
	fn new(size: usize) -> Self {
		let boxed: Box<[u8]> = vec![0u8; size].into_boxed_slice();
		let capacity = boxed.len();
		let data = Box::into_raw(boxed) as *mut u8;
		Self { data, capacity }
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

	/// The mutable buffer for multi-chunk writes, lazily allocated.
	///
	/// Returns `None` once a whole-frame write has installed shared storage.
	fn mutable(&self) -> Option<&MutableFrameBuf> {
		match self
			.0
			.storage
			.get_or_init(|| FrameStorage::Mutable(MutableFrameBuf::new(self.capacity())))
		{
			FrameStorage::Shared(_) => None,
			FrameStorage::Mutable(buf) => Some(buf),
		}
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
	fn append(&self, src: &[u8]) {
		if src.is_empty() {
			return;
		}
		let prev = self.written(Ordering::Relaxed);
		let Some(buf) = self.mutable() else {
			// Only reachable if the frame is already complete via shared storage, which
			// `Producer::write` rejects for a non-empty chunk. Nothing to copy.
			return;
		};
		// Safety: sole writer; the caller bounds-checked `src` against the remaining
		// capacity, and consumers only read `[..written]`.
		unsafe {
			std::ptr::copy_nonoverlapping(src.as_ptr(), buf.data.add(prev), src.len());
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
				unsafe { std::slice::from_raw_parts(buf.data, written) }
			}
			None => &[],
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
pub struct Producer<'a> {
	group: &'a mut group::Producer,
	buf: FrameBuf,
	info: Info,
	// Set once the frame is committed (finished) or aborted, so Drop is a no-op.
	done: bool,
}

impl std::ops::Deref for Producer<'_> {
	type Target = Info;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl<'a> Producer<'a> {
	pub(crate) fn new(group: &'a mut group::Producer, buf: FrameBuf, info: Info) -> Self {
		Self {
			group,
			buf,
			info,
			done: false,
		}
	}

	/// The parent group this frame belongs to.
	pub fn group(&self) -> group::Info {
		self.group.info()
	}

	/// Bytes still needed to complete the frame.
	pub fn remaining(&self) -> usize {
		self.buf.capacity() - self.buf.written(Ordering::Acquire)
	}

	/// Write a chunk of data to the frame.
	///
	/// Returns [`Error::WrongSize`] if the chunk would exceed the remaining bytes.
	pub fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		let len = chunk.as_ref().len();
		if len > self.remaining() {
			return Err(Error::WrongSize);
		}
		// Fast path: a single whole-frame write keeps the caller's allocation.
		if len == self.buf.capacity() && self.buf.written(Ordering::Acquire) == 0 {
			match self.buf.try_set_bytes(chunk.into_bytes()) {
				Ok(()) => {
					let cap = self.buf.capacity();
					// Safety: `try_set_bytes` checked the buffer exactly matches the declared
					// size, so publishing all bytes is in bounds.
					unsafe { self.buf.store_written(cap) };
				}
				Err(chunk) => self.buf.append(&chunk),
			}
		} else {
			self.buf.append(chunk.as_ref());
		}
		self.group.frame_notify();
		Ok(())
	}

	/// Commit the frame, verifying that all bytes were written.
	///
	/// Returns [`Error::WrongSize`] if the bytes written don't match [`Info::size`].
	pub fn finish(mut self) -> Result<()> {
		if self.buf.written(Ordering::Acquire) != self.buf.capacity() {
			return Err(Error::WrongSize);
		}
		let payload = self.buf.freeze(self.buf.capacity());
		self.group.frame_commit(Frame {
			timestamp: self.info.timestamp,
			payload,
		})?;
		self.done = true;
		Ok(())
	}

	/// Abort the frame (and its group) with the given error.
	pub fn abort(mut self, err: Error) -> Result<()> {
		self.group.frame_abort(err);
		self.done = true;
		Ok(())
	}
}

impl Drop for Producer<'_> {
	fn drop(&mut self) {
		if !self.done {
			// An unfinished frame leaves the group stream broken; fail the group so
			// consumers surface an error instead of hanging on the partial forever.
			tracing::warn!(
				group = self.group.info().sequence,
				"frame::Producer dropped before writing all bytes"
			);
			self.group.frame_abort(Error::Dropped);
		}
	}
}

/// The source of a [`Consumer`]'s payload: a finished frame (whole) or the in-flight
/// tail (streamed).
#[derive(Clone)]
pub(crate) enum Source {
	Complete(Bytes),
	Partial(FrameBuf),
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
		}
	}

	/// Poll for the next chunk of bytes since the last read.
	///
	/// Returns `None` once the frame is finished and all bytes have been consumed.
	pub fn poll_read_chunk(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		match &self.source {
			Source::Complete(bytes) => {
				if self.read_idx >= bytes.len() {
					return Poll::Ready(Ok(None));
				}
				let out = bytes.slice(self.read_idx..);
				self.read_idx = bytes.len();
				Poll::Ready(Ok(Some(out)))
			}
			Source::Partial(buf) => {
				let buf = buf.clone();
				let size = self.info.size as usize;
				loop {
					let written = buf.written(Ordering::Acquire);
					if written > self.read_idx {
						let out = buf.slice(self.read_idx, written);
						self.read_idx = written;
						return Poll::Ready(Ok(Some(out)));
					}
					if written >= size {
						return Poll::Ready(Ok(None));
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
		}
	}

	/// Return the next chunk of bytes since the last read.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_read_chunk(waiter)).await
	}

	/// Poll for all remaining bytes, resolving once the frame is finished.
	pub fn poll_read_all(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes>> {
		match &self.source {
			Source::Complete(bytes) => {
				let out = bytes.slice(self.read_idx..);
				self.read_idx = bytes.len();
				Poll::Ready(Ok(out))
			}
			Source::Partial(buf) => {
				let buf = buf.clone();
				let size = self.info.size as usize;
				let read_idx = self.read_idx;
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
				Poll::Ready(Ok(out))
			}
		}
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
