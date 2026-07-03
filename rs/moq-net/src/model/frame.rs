use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Poll, ready};

use bytes::buf::UninitSlice;
use bytes::{BufMut, Bytes};

use crate::{IntoBytes, Error, GroupInfo, Result, Timestamp};

/// Maximum payload size accepted for a single frame.
///
/// The receive path trusts the declared frame size when storing the payload, so an
/// untrusted peer could otherwise request a multi-gigabyte allocation with a
/// single varint. [`FrameProducer::new`] enforces this for every frame and
/// rejects an oversized declared size with [`Error::FrameTooLarge`] before the
/// payload is stored.
///
/// Matches the per-group cache cap (`MAX_GROUP_CACHE`), so a single frame may fill
/// a group. 16 MiB was too tight for a high-bitrate CMAF fragment carried as one
/// frame; 32 MiB covers that while keeping the per-frame preallocation bounded.
pub(crate) const MAX_FRAME_SIZE: u64 = 32 * 1024 * 1024;

/// A chunk of data with an upfront size and a presentation timestamp.
///
/// Note that this is just the header.
/// You use [FrameProducer] and [FrameConsumer] to deal with the frame payload, potentially chunked.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrameInfo {
	/// Total payload size in bytes. Declared up front so consumers can preallocate.
	pub size: u64,
	/// Presentation timestamp.
	///
	/// [`crate::GroupProducer::create_frame`] converts it into the parent track's
	/// timescale, so the scale you build it with doesn't have to match the track.
	/// Use [`crate::GroupProducer::create_frame_now`] /
	/// [`crate::GroupProducer::write_frame_now`] to stamp wall-clock time instead of
	/// supplying one explicitly.
	pub timestamp: Timestamp,
}

impl FrameInfo {
	/// Create an unparented producer for the frame.
	///
	/// Test-only: real frames are constructed via
	/// [`crate::GroupProducer::create_frame`], which threads the parent
	/// [`GroupInfo`] down and validates the timestamp against the track's
	/// timescale. This helper defaults the parent group for in-crate tests. Returns
	/// [`Error::FrameTooLarge`] if [`FrameInfo::size`] exceeds [`MAX_FRAME_SIZE`].
	#[cfg(test)]
	pub(crate) fn produce(self) -> Result<FrameProducer> {
		FrameProducer::new(self, GroupInfo { sequence: 0 })
	}
}

/// Payload storage shared between a [FrameProducer] and many [FrameConsumer]s.
///
/// Whole-frame [`Bytes`] writes are stored directly. Partial writes and [`BufMut`]
/// writes fall back to one mutable heap allocation sized to the declared frame.
///
/// The producer writes through the raw pointer (sole writer); `written` provides
/// happens-before for cross-thread reads. Implements [AsRef]<[u8]> directly so it
/// can be passed to [Bytes::from_owner] without an extra wrapper newtype.
#[derive(Clone)]
struct FrameBuf(Arc<FrameBufInner>);

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

// Safety: `data` is owned (Box-allocated, freed in Drop). The producer is the
// sole writer and consumers only read bytes `< written`.
unsafe impl Send for MutableFrameBuf {}
unsafe impl Sync for MutableFrameBuf {}

impl Drop for MutableFrameBuf {
	fn drop(&mut self) {
		// Safety: data was obtained from `Box::into_raw` of a `Box<[u8]>` of
		// length `capacity` and is not aliased at drop (Arc refcount hit 0).
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
	fn new(size: usize) -> Self {
		Self(Arc::new(FrameBufInner {
			capacity: size,
			written: AtomicUsize::new(0),
			storage: OnceLock::new(),
		}))
	}

	fn capacity(&self) -> usize {
		self.0.capacity
	}

	fn written(&self, ord: Ordering) -> usize {
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

	/// The mutable buffer for partial and [`BufMut`] writes, lazily allocated.
	///
	/// Returns `None` once a whole-frame write has installed shared storage: the
	/// frame is already complete, so there's no writable region left. A correct
	/// [`BufMut`] caller never reaches that state (`remaining_mut` is 0), but we
	/// return `None` rather than panic if one does.
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
}

impl AsRef<[u8]> for FrameBuf {
	fn as_ref(&self) -> &[u8] {
		// Snapshot the initialized region (bytes the producer has written so far).
		// Acquire pairs with the producer's Release on `written`.
		let written = self.0.written.load(Ordering::Acquire);
		match self.0.storage.get() {
			Some(FrameStorage::Shared(bytes)) => &bytes[..written],
			Some(FrameStorage::Mutable(buf)) => {
				// Safety: data..data+written is initialized (zero-init at alloc +
				// producer writes up to `written`). The Arc keeps the allocation alive
				// while any reference to the slice lives.
				unsafe { std::slice::from_raw_parts(buf.data, written) }
			}
			None => &[],
		}
	}
}

#[derive(Default, Debug)]
struct FrameState {
	// Whether the producer signaled a clean finish (written == capacity).
	fin: bool,
	// The error that aborted the frame, if any.
	abort: Option<Error>,
}

/// Writes a frame's payload in one or more chunks.
///
/// The total bytes written must exactly match [FrameInfo::size].
/// Call [Self::finish] after writing all bytes to verify correctness.
///
/// Implements [BufMut] so the receive path can write directly into the
/// pre-allocated buffer (e.g. via `tokio::io::AsyncReadExt::read_buf`).
pub struct FrameProducer {
	info: FrameInfo,
	// The parent group's info, inherited from [`crate::GroupProducer::create_frame`]
	// so the ownership chain reaches the leaf. A small `Copy` value; carried for
	// identity/debugging (the timestamp-vs-timescale check lives on the group).
	group: GroupInfo,
	state: kio::Producer<FrameState>,
	buf: FrameBuf,
}

impl std::ops::Deref for FrameProducer {
	type Target = FrameInfo;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl FrameProducer {
	/// Create a new frame producer for the given frame header.
	///
	/// The payload storage chokepoint: rejects a frame whose declared
	/// [`FrameInfo::size`] exceeds [`MAX_FRAME_SIZE`] with [`Error::FrameTooLarge`]
	/// before storing the untrusted payload.
	pub(crate) fn new(info: FrameInfo, group: GroupInfo) -> Result<Self> {
		if info.size > MAX_FRAME_SIZE {
			return Err(Error::FrameTooLarge);
		}
		let buf = FrameBuf::new(info.size as usize);
		Ok(Self {
			info,
			group,
			state: kio::Producer::new(FrameState::default()),
			buf,
		})
	}

	/// The parent group this frame belongs to.
	pub fn group(&self) -> &GroupInfo {
		&self.group
	}

	/// Write a chunk of data to the frame.
	///
	/// Returns [Error::WrongSize] if the chunk would exceed the remaining bytes.
	pub fn write<B: IntoBytes>(&mut self, chunk: B) -> Result<()> {
		let len = chunk.as_ref().len();
		if len > self.remaining_mut() {
			return Err(Error::WrongSize);
		}
		// Surface aborts before writing.
		self.bail_if_aborted()?;
		if len == self.buf.capacity() && self.buf.written(Ordering::Acquire) == 0 {
			match self.buf.try_set_bytes(chunk.into_bytes()) {
				Ok(()) => {
					let cap = self.buf.capacity();
					// Safety: `try_set_bytes` checked that the buffer exactly matches
					// the declared size, so publishing all bytes is within bounds.
					unsafe { self.buf.store_written(cap) };
					self.notify_written(cap);
					return Ok(());
				}
				Err(chunk) => {
					self.put_slice(&chunk);
					return Ok(());
				}
			}
		}
		self.put_slice(chunk.as_ref());
		Ok(())
	}

	/// Verify that all bytes have been written.
	///
	/// Returns [Error::WrongSize] if the bytes written don't match [FrameInfo::size].
	pub fn finish(&mut self) -> Result<()> {
		let written = self.buf.written(Ordering::Acquire);
		if written != self.buf.capacity() {
			return Err(Error::WrongSize);
		}
		// Mark fin (idempotent if `advance_mut` already set it on the last byte).
		let mut state = self.modify()?;
		state.fin = true;
		Ok(())
	}

	/// Abort the frame with the given error.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut guard = self.modify()?;
		guard.abort = Some(err);
		guard.close();
		Ok(())
	}

	/// Create a new consumer for the frame.
	pub fn consume(&self) -> FrameConsumer {
		FrameConsumer {
			info: self.info,
			state: self.state.consume(),
			buf: self.buf.clone(),
			read_idx: 0,
		}
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	fn modify(&mut self) -> Result<kio::Mut<'_, FrameState>> {
		self.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	fn bail_if_aborted(&self) -> Result<()> {
		let state = self.state.read();
		if let Some(err) = &state.abort {
			return Err(err.clone());
		}
		Ok(())
	}

	fn notify_written(&mut self, written: usize) {
		// Briefly take the kio write lock to wake waiters; drop of `Mut` triggers
		// kio's notify. Also flip `fin` if we just filled the buffer.
		if let Ok(mut state) = self.state.write()
			&& written == self.buf.capacity()
		{
			state.fin = true;
		}
	}
}

// Safety: `chunk_mut` returns a slice into the producer-private region of the
// buffer (`[written..capacity]`). Sole-writer invariant: even though
// `FrameProducer` is `Clone`, the API exposes BufMut only via `&mut self`,
// and existing callers never share a single producer between concurrent writers
// (group.rs clones a handle for `abort` / `consume` only). The defensive
// `assert!` in `advance_mut` panics loudly if that invariant is ever violated.
unsafe impl BufMut for FrameProducer {
	fn remaining_mut(&self) -> usize {
		self.buf.capacity() - self.buf.written(Ordering::Acquire)
	}

	fn chunk_mut(&mut self) -> &mut UninitSlice {
		let written = self.buf.written(Ordering::Acquire);
		let cap = self.buf.capacity();
		let Some(buf) = self.buf.mutable() else {
			// A whole-frame write already completed the frame via shared storage,
			// so there's no writable region. `remaining_mut` is 0 here, so a
			// well-behaved caller never advances into this empty slice; a misbehaving
			// one trips the bounds `assert!` in `advance_mut`.
			//
			// Safety: a zero-length slice over a dangling-but-aligned pointer is
			// never dereferenced.
			return unsafe { UninitSlice::from_raw_parts_mut(std::ptr::NonNull::dangling().as_ptr(), 0) };
		};
		// Safety: writes to `[written..cap]` are unaliased — consumers only ever
		// read `[..written]`, and we hold `&mut self`. The slice's lifetime is
		// tied to `&mut self` by the function signature.
		unsafe {
			let ptr = buf.data.add(written);
			UninitSlice::from_raw_parts_mut(ptr, cap - written)
		}
	}

	unsafe fn advance_mut(&mut self, cnt: usize) {
		let cap = self.buf.capacity();
		let prev = self.buf.written(Ordering::Relaxed);
		assert!(
			prev + cnt <= cap,
			"advance_mut past frame.size: prev={prev} cnt={cnt} cap={cap}"
		);
		// Safety: sole-writer invariant + bounds-checked above.
		unsafe { self.buf.store_written(prev + cnt) };
		self.notify_written(prev + cnt);
	}
}

impl Clone for FrameProducer {
	fn clone(&self) -> Self {
		Self {
			info: self.info,
			group: self.group,
			state: self.state.clone(),
			buf: self.buf.clone(),
		}
	}
}

/// Used to consume a frame's worth of data, streaming as bytes arrive.
#[derive(Clone)]
pub struct FrameConsumer {
	info: FrameInfo,
	state: kio::Consumer<FrameState>,
	buf: FrameBuf,
	// Byte offset into the buffer; cloned consumers inherit this offset and
	// read independently from there.
	read_idx: usize,
}

impl std::ops::Deref for FrameConsumer {
	type Target = FrameInfo;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl FrameConsumer {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&kio::Ref<'_, FrameState>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	fn snapshot(&self, read_idx: usize) -> Option<Bytes> {
		// Acquire pairs with the producer's Release on `written`, making the
		// bytes in `[..written]` visible to this thread.
		let written = self.buf.written(Ordering::Acquire);
		if written > read_idx {
			Some(Bytes::from_owner(self.buf.clone()).slice(read_idx..written))
		} else {
			None
		}
	}

	/// Poll for all remaining data without blocking.
	///
	/// Waits until the frame is finished (written == size); then returns the
	/// remaining bytes from `read_idx` to the end as a single zero-copy slice.
	pub fn poll_read_all(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes>> {
		let read_idx = self.read_idx;
		let res = ready!(self.poll(waiter, |state| {
			if state.fin {
				return Poll::Ready(Ok(()));
			}
			if let Some(err) = &state.abort {
				return Poll::Ready(Err(err.clone()));
			}
			Poll::Pending
		}));
		match res {
			Ok(()) => {
				// Frame is finished: written == capacity.
				let bytes = self
					.snapshot(read_idx)
					.unwrap_or_else(|| Bytes::from_owner(self.buf.clone()).slice(read_idx..read_idx));
				self.read_idx = self.buf.capacity();
				Poll::Ready(Ok(bytes))
			}
			Err(e) => Poll::Ready(Err(e)),
		}
	}

	/// Return all of the remaining bytes, blocking until the frame is finished.
	pub async fn read_all(&mut self) -> Result<Bytes> {
		kio::wait(|waiter| self.poll_read_all(waiter)).await
	}

	/// Poll for all remaining bytes (split into a single-element vec for backwards
	/// compatibility with the previous chunk-based API).
	pub fn poll_read_all_chunks(&mut self, waiter: &kio::Waiter) -> Poll<Result<Vec<Bytes>>> {
		let bytes = ready!(self.poll_read_all(waiter)?);
		Poll::Ready(Ok(if bytes.is_empty() { Vec::new() } else { vec![bytes] }))
	}

	/// Poll for the next chunk of bytes since the last read.
	///
	/// Returns whatever bytes have been written since the consumer's `read_idx` —
	/// could span multiple producer writes. Returns `None` once the frame is
	/// finished and all bytes have been consumed.
	pub fn poll_read_chunk(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		let read_idx = self.read_idx;
		let res = ready!(self.poll(waiter, |state| {
			let written = self.buf.written(Ordering::Acquire);
			if written > read_idx {
				return Poll::Ready(Ok(Some(written)));
			}
			if state.fin {
				return Poll::Ready(Ok(None));
			}
			if let Some(err) = &state.abort {
				return Poll::Ready(Err(err.clone()));
			}
			Poll::Pending
		}));
		match res {
			Ok(Some(written)) => {
				let bytes = Bytes::from_owner(self.buf.clone()).slice(read_idx..written);
				self.read_idx = written;
				Poll::Ready(Ok(Some(bytes)))
			}
			Ok(None) => Poll::Ready(Ok(None)),
			Err(e) => Poll::Ready(Err(e)),
		}
	}

	/// Return the next chunk of bytes since the last read.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_read_chunk(waiter)).await
	}

	/// Poll for the next chunk; for backwards compatibility, wraps
	/// [Self::poll_read_chunk] in a vec (single element if any data is available).
	pub fn poll_read_chunks(&mut self, waiter: &kio::Waiter) -> Poll<Result<Vec<Bytes>>> {
		match ready!(self.poll_read_chunk(waiter)?) {
			Some(b) => Poll::Ready(Ok(vec![b])),
			None => Poll::Ready(Ok(Vec::new())),
		}
	}

	/// Read the next chunk into a vector (single element if available, empty on eof).
	pub async fn read_chunks(&mut self) -> Result<Vec<Bytes>> {
		kio::wait(|waiter| self.poll_read_chunks(waiter)).await
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	#[test]
	fn single_chunk_roundtrip() {
		let mut producer = FrameInfo {
			size: 5,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"hello"));
	}

	#[test]
	fn whole_bytes_write_reuses_allocation() {
		let input = Bytes::from(vec![1, 2, 3, 4, 5]);
		let input_ptr = input.as_ptr();
		let mut producer = FrameInfo {
			size: input.len() as u64,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(input.clone()).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, input);
		assert_eq!(data.as_ptr(), input_ptr);
	}

	// A whole-frame write installs shared storage and completes the frame. Reaching
	// for the BufMut region afterwards must not panic: there's simply nothing left
	// to write.
	#[test]
	fn bufmut_after_whole_frame_write_does_not_panic() {
		let mut producer = FrameInfo {
			size: 3,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(Bytes::from_static(b"abc")).unwrap();

		assert_eq!(bytes::BufMut::remaining_mut(&producer), 0);
		assert_eq!(bytes::BufMut::chunk_mut(&mut producer).len(), 0);

		producer.finish().unwrap();
		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"abc"));
	}

	#[test]
	fn multi_chunk_read_all() {
		let mut producer = FrameInfo {
			size: 10,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(Bytes::from_static(b"hello")).unwrap();
		producer.write(Bytes::from_static(b"world")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"helloworld"));
	}

	#[test]
	fn read_chunk_sequential() {
		let mut producer = FrameInfo {
			size: 10,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(Bytes::from_static(b"hello")).unwrap();
		// Each read_chunk returns whatever is new since the last call,
		// which may span multiple writes.
		let mut consumer = producer.consume();
		let c1 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c1, Some(Bytes::from_static(b"hello")));

		producer.write(Bytes::from_static(b"world")).unwrap();
		producer.finish().unwrap();

		let c2 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c2, Some(Bytes::from_static(b"world")));
		let c3 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c3, None);
	}

	#[test]
	fn read_all_chunks() {
		let mut producer = FrameInfo {
			size: 10,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(Bytes::from_static(b"hello")).unwrap();
		producer.write(Bytes::from_static(b"world")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let chunks = consumer.read_chunks().now_or_never().unwrap().unwrap();
		assert_eq!(chunks.len(), 1);
		assert_eq!(chunks[0], Bytes::from_static(b"helloworld"));
	}

	#[test]
	fn finish_checks_remaining() {
		let mut producer = FrameInfo {
			size: 5,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.write(Bytes::from_static(b"hi")).unwrap();
		let err = producer.finish().unwrap_err();
		assert!(matches!(err, Error::WrongSize));
	}

	#[test]
	fn write_too_many_bytes() {
		let mut producer = FrameInfo {
			size: 3,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		let err = producer.write(Bytes::from_static(b"toolong")).unwrap_err();
		assert!(matches!(err, Error::WrongSize));
	}

	#[test]
	fn rejects_oversized_frame() {
		// The allocation chokepoint refuses an oversized declared size before any
		// buffer is allocated, so a single varint can't request a huge allocation.
		let result = FrameInfo {
			size: MAX_FRAME_SIZE + 1,
			timestamp: Timestamp::ZERO,
		}
		.produce();
		assert!(matches!(result, Err(Error::FrameTooLarge)));
	}

	#[test]
	fn abort_propagates() {
		let mut producer = FrameInfo {
			size: 5,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		let mut consumer = producer.consume();
		producer.abort(Error::Cancel).unwrap();

		let err = consumer.read_all().now_or_never().unwrap().unwrap_err();
		assert!(matches!(err, Error::Cancel));
	}

	#[test]
	fn empty_frame() {
		let mut producer = FrameInfo {
			size: 0,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::new());
	}

	#[tokio::test]
	async fn pending_then_ready() {
		let mut producer = FrameInfo {
			size: 5,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		let mut consumer = producer.consume();

		// Consumer blocks because no data yet.
		assert!(consumer.read_all().now_or_never().is_none());

		producer.write(Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"hello"));
	}

	#[test]
	fn buf_mut_roundtrip() {
		// Exercise the BufMut path that the receive loop uses via `read_buf`.
		let mut producer = FrameInfo {
			size: 12,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		assert_eq!(producer.remaining_mut(), 12);
		producer.put_slice(b"hello");
		assert_eq!(producer.remaining_mut(), 7);
		producer.put_slice(b" world!");
		assert_eq!(producer.remaining_mut(), 0);
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"hello world!"));
	}

	#[test]
	#[should_panic(expected = "advance_mut past frame.size")]
	fn buf_mut_advance_past_capacity_panics() {
		let mut producer = FrameInfo {
			size: 4,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		// Safety violation on purpose: cnt > remaining_mut().
		unsafe { producer.advance_mut(5) };
	}

	#[test]
	fn read_chunk_streams_partial_writes() {
		let mut producer = FrameInfo {
			size: 6,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		let mut consumer = producer.consume();

		producer.write(Bytes::from_static(b"foo")).unwrap();
		let c1 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c1, Some(Bytes::from_static(b"foo")));

		// No new data → pending.
		assert!(consumer.read_chunk().now_or_never().is_none());

		producer.write(Bytes::from_static(b"bar")).unwrap();
		producer.finish().unwrap();
		let c2 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c2, Some(Bytes::from_static(b"bar")));
		let c3 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c3, None);
	}

	#[test]
	fn cloned_consumer_independent_cursor() {
		let mut producer = FrameInfo {
			size: 10,
			timestamp: Timestamp::ZERO,
		}
		.produce()
		.unwrap();
		let mut c1 = producer.consume();
		producer.write(Bytes::from_static(b"hello")).unwrap();

		// c1 reads the first 5 bytes, then we clone — c2 inherits c1's cursor.
		let chunk = c1.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(chunk, Some(Bytes::from_static(b"hello")));
		let mut c2 = c1.clone();

		producer.write(Bytes::from_static(b"world")).unwrap();
		producer.finish().unwrap();

		// Both consumers now see "world" as their next chunk.
		let chunk = c1.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(chunk, Some(Bytes::from_static(b"world")));
		let chunk = c2.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(chunk, Some(Bytes::from_static(b"world")));
	}
}
