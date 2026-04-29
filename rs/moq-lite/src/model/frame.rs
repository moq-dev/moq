use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Poll, ready};

use bytes::buf::UninitSlice;
use bytes::{BufMut, Bytes};

use crate::{Error, Result};

/// A chunk of data with an upfront size.
///
/// Note that this is just the header.
/// You use [FrameProducer] and [FrameConsumer] to deal with the frame payload, potentially chunked.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frame {
	pub size: u64,
}

impl Frame {
	/// Create a new producer for the frame.
	pub fn produce(self) -> FrameProducer {
		FrameProducer::new(self)
	}
}

impl From<usize> for Frame {
	fn from(size: usize) -> Self {
		Self { size: size as u64 }
	}
}

impl From<u64> for Frame {
	fn from(size: u64) -> Self {
		Self { size }
	}
}

impl From<u32> for Frame {
	fn from(size: u32) -> Self {
		Self { size: size as u64 }
	}
}

impl From<u16> for Frame {
	fn from(size: u16) -> Self {
		Self { size: size as u64 }
	}
}

/// Single-allocation buffer shared between a [FrameProducer] and many [FrameConsumer]s.
///
/// Internally an [Arc] over a thin pointer + length owning a heap allocation. The
/// data pointer is stable for the life of any clone, so [Bytes] views taken via
/// [Bytes::from_owner] remain valid. [Clone] is cheap (one atomic increment).
///
/// The producer writes through the raw pointer (sole writer); `written` provides
/// happens-before for cross-thread reads. Implements [AsRef]<[u8]> directly so it
/// can be passed to [Bytes::from_owner] without an extra wrapper newtype.
#[derive(Clone)]
struct FrameBuf(Arc<FrameBufInner>);

struct FrameBufInner {
	// Owned heap allocation of `capacity` bytes (zero-initialized).
	data: *mut u8,
	capacity: usize,
	written: AtomicUsize,
}

// Safety: `data` is owned (Box-allocated, freed in Drop); the producer is the
// sole writer; consumers only read bytes `< written`, which was set via Release
// after the corresponding writes completed (Acquire pairs on the consumer side).
unsafe impl Send for FrameBufInner {}
unsafe impl Sync for FrameBufInner {}

impl Drop for FrameBufInner {
	fn drop(&mut self) {
		// Safety: data was obtained from `Box::into_raw` of a `Box<[u8]>` of
		// length `capacity` and is not aliased at drop (Arc refcount hit 0).
		unsafe {
			let slice = std::ptr::slice_from_raw_parts_mut(self.data, self.capacity);
			drop(Box::from_raw(slice));
		}
	}
}

impl FrameBuf {
	fn new(size: usize) -> Self {
		let boxed: Box<[u8]> = vec![0u8; size].into_boxed_slice();
		let capacity = boxed.len();
		let data = Box::into_raw(boxed) as *mut u8;
		Self(Arc::new(FrameBufInner {
			data,
			capacity,
			written: AtomicUsize::new(0),
		}))
	}

	fn capacity(&self) -> usize {
		self.0.capacity
	}

	fn written(&self, ord: Ordering) -> usize {
		self.0.written.load(ord)
	}

	/// Safety: caller must be the sole producer (FrameProducer-as-BufMut invariant).
	unsafe fn data_ptr(&self) -> *mut u8 {
		self.0.data
	}

	/// Safety: caller must be the sole producer; `new_written` must be `<= capacity`.
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
		// Safety: data..data+written is initialized (zero-init at alloc + producer
		// writes up to `written`). The Arc keeps the allocation alive while any
		// reference to the slice lives.
		unsafe { std::slice::from_raw_parts(self.0.data, written) }
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
/// The total bytes written must exactly match [Frame::size].
/// Call [Self::finish] after writing all bytes to verify correctness.
///
/// Implements [BufMut] so the receive path can write directly into the
/// pre-allocated buffer (e.g. via `tokio::io::AsyncReadExt::read_buf`).
pub struct FrameProducer {
	info: Frame,
	state: conducer::Producer<FrameState>,
	buf: FrameBuf,
}

impl std::ops::Deref for FrameProducer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl FrameProducer {
	/// Create a new frame producer for the given frame header.
	pub fn new(info: Frame) -> Self {
		let buf = FrameBuf::new(info.size as usize);
		Self {
			info,
			state: conducer::Producer::new(FrameState::default()),
			buf,
		}
	}

	/// Write a chunk of data to the frame.
	///
	/// Returns [Error::WrongSize] if the chunk would exceed the remaining bytes.
	pub fn write<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		let chunk = chunk.into();
		if chunk.len() > self.remaining_mut() {
			return Err(Error::WrongSize);
		}
		// Surface aborts before writing.
		self.bail_if_aborted()?;
		self.put_slice(&chunk);
		Ok(())
	}

	/// Deprecated: use [`Self::write`] instead.
	#[deprecated(note = "use write(chunk) instead")]
	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		self.write(chunk)
	}

	/// Verify that all bytes have been written.
	///
	/// Returns [Error::WrongSize] if the bytes written don't match [Frame::size].
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
			info: self.info.clone(),
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

	fn modify(&mut self) -> Result<conducer::Mut<'_, FrameState>> {
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
		// Safety: writes to `[written..cap]` are unaliased — consumers only ever
		// read `[..written]`, and we hold `&mut self`. The slice's lifetime is
		// tied to `&mut self` by the function signature.
		unsafe {
			let ptr = self.buf.data_ptr().add(written);
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

		// Briefly take the conducer write lock to wake waiters; drop of `Mut`
		// triggers conducer's notify. Also flip `fin` if we just filled the buffer.
		if let Ok(mut state) = self.state.write() {
			if prev + cnt == cap {
				state.fin = true;
			}
		}
	}
}

impl Clone for FrameProducer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			state: self.state.clone(),
			buf: self.buf.clone(),
		}
	}
}

impl From<Frame> for FrameProducer {
	fn from(info: Frame) -> Self {
		FrameProducer::new(info)
	}
}

/// Used to consume a frame's worth of data, streaming as bytes arrive.
#[derive(Clone)]
pub struct FrameConsumer {
	info: Frame,
	state: conducer::Consumer<FrameState>,
	buf: FrameBuf,
	// Byte offset into the buffer; cloned consumers inherit this offset and
	// read independently from there.
	read_idx: usize,
}

impl std::ops::Deref for FrameConsumer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl FrameConsumer {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&conducer::Ref<'_, FrameState>) -> Poll<Result<R>>,
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
	pub fn poll_read_all(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Bytes>> {
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
		conducer::wait(|waiter| self.poll_read_all(waiter)).await
	}

	/// Poll for all remaining bytes (split into a single-element vec for backwards
	/// compatibility with the previous chunk-based API).
	pub fn poll_read_all_chunks(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Vec<Bytes>>> {
		let bytes = ready!(self.poll_read_all(waiter)?);
		Poll::Ready(Ok(if bytes.is_empty() { Vec::new() } else { vec![bytes] }))
	}

	/// Poll for the next chunk of bytes since the last read.
	///
	/// Returns whatever bytes have been written since the consumer's `read_idx` —
	/// could span multiple producer writes. Returns `None` once the frame is
	/// finished and all bytes have been consumed.
	pub fn poll_read_chunk(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Bytes>>> {
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
		conducer::wait(|waiter| self.poll_read_chunk(waiter)).await
	}

	/// Poll for the next chunk; for backwards compatibility, wraps
	/// [Self::poll_read_chunk] in a vec (single element if any data is available).
	pub fn poll_read_chunks(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Vec<Bytes>>> {
		match ready!(self.poll_read_chunk(waiter)?) {
			Some(b) => Poll::Ready(Ok(vec![b])),
			None => Poll::Ready(Ok(Vec::new())),
		}
	}

	/// Read the next chunk into a vector (single element if available, empty on eof).
	pub async fn read_chunks(&mut self) -> Result<Vec<Bytes>> {
		conducer::wait(|waiter| self.poll_read_chunks(waiter)).await
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	#[test]
	fn single_chunk_roundtrip() {
		let mut producer = Frame { size: 5 }.produce();
		producer.write(Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"hello"));
	}

	#[test]
	fn multi_chunk_read_all() {
		let mut producer = Frame { size: 10 }.produce();
		producer.write(Bytes::from_static(b"hello")).unwrap();
		producer.write(Bytes::from_static(b"world")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"helloworld"));
	}

	#[test]
	fn read_chunk_sequential() {
		let mut producer = Frame { size: 10 }.produce();
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
		let mut producer = Frame { size: 10 }.produce();
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
		let mut producer = Frame { size: 5 }.produce();
		producer.write(Bytes::from_static(b"hi")).unwrap();
		let err = producer.finish().unwrap_err();
		assert!(matches!(err, Error::WrongSize));
	}

	#[test]
	fn write_too_many_bytes() {
		let mut producer = Frame { size: 3 }.produce();
		let err = producer.write(Bytes::from_static(b"toolong")).unwrap_err();
		assert!(matches!(err, Error::WrongSize));
	}

	#[test]
	fn abort_propagates() {
		let mut producer = Frame { size: 5 }.produce();
		let mut consumer = producer.consume();
		producer.abort(Error::Cancel).unwrap();

		let err = consumer.read_all().now_or_never().unwrap().unwrap_err();
		assert!(matches!(err, Error::Cancel));
	}

	#[test]
	fn empty_frame() {
		let mut producer = Frame { size: 0 }.produce();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_all().now_or_never().unwrap().unwrap();
		assert_eq!(data, Bytes::new());
	}

	#[tokio::test]
	async fn pending_then_ready() {
		let mut producer = Frame { size: 5 }.produce();
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
		let mut producer = Frame { size: 12 }.produce();
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
		let mut producer = Frame { size: 4 }.produce();
		// Safety violation on purpose: cnt > remaining_mut().
		unsafe { producer.advance_mut(5) };
	}

	#[test]
	fn read_chunk_streams_partial_writes() {
		let mut producer = Frame { size: 6 }.produce();
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
		let mut producer = Frame { size: 10 }.produce();
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
