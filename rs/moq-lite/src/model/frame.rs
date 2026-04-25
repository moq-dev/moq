use std::task::{Poll, ready};

use bytes::{Bytes, BytesMut};

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

#[derive(Default, Debug)]
struct FrameState {
	// The chunks that have been written thus far
	chunks: Vec<Bytes>,

	// The number of bytes remaining to be written.
	remaining: u64,

	// The error that caused the frame to be aborted, if any.
	abort: Option<Error>,
}

impl FrameState {
	fn write_chunk(&mut self, chunk: Bytes) -> Result<()> {
		if let Some(err) = &self.abort {
			return Err(err.clone());
		}

		self.remaining = self.remaining.checked_sub(chunk.len() as u64).ok_or(Error::WrongSize)?;
		self.chunks.push(chunk);
		Ok(())
	}

	fn poll_read_chunk(&self, index: usize) -> Poll<Result<Option<Bytes>>> {
		if let Some(chunk) = self.chunks.get(index).cloned() {
			Poll::Ready(Ok(Some(chunk)))
		} else if self.remaining == 0 {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn poll_read_chunks(&self, index: usize) -> Poll<Result<Vec<Bytes>>> {
		if index >= self.chunks.len() && self.remaining == 0 {
			Poll::Ready(Ok(Vec::new()))
		} else if self.remaining == 0 {
			Poll::Ready(Ok(self.chunks[index..].to_vec()))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn poll_read_all(&self, index: usize) -> Poll<Result<Bytes>> {
		let chunks = ready!(self.poll_read_all_chunks(index)?);

		Poll::Ready(Ok(match chunks.len() {
			0 => Bytes::new(),
			1 => chunks[0].clone(),
			_ => {
				let size = chunks.iter().map(Bytes::len).sum();
				let mut buf = BytesMut::with_capacity(size);
				for chunk in chunks {
					buf.extend_from_slice(chunk.as_ref());
				}
				buf.freeze()
			}
		}))
	}

	fn poll_read_all_chunks(&self, index: usize) -> Poll<Result<&[Bytes]>> {
		if self.remaining > 0 {
			Poll::Pending
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else if index < self.chunks.len() {
			Poll::Ready(Ok(&self.chunks[index..]))
		} else {
			Poll::Ready(Ok(&[]))
		}
	}
}

/// Writes a frame's payload in one or more chunks.
///
/// The total bytes written must exactly match [Frame::size].
/// Call [Self::finish] after writing all bytes to verify correctness.
pub struct FrameProducer {
	// The frame header containing the expected size.
	info: Frame,

	// Mutable stream state.
	state: conducer::Producer<FrameState>,
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
		let state = FrameState {
			chunks: Vec::new(),
			remaining: info.size,
			abort: None,
		};
		Self {
			info,
			state: conducer::Producer::new(state),
		}
	}

	/// Write a chunk of data to the frame.
	///
	/// Returns [Error::WrongSize] if the total bytes written would exceed [Frame::size].
	pub fn write<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		let chunk = chunk.into();
		let mut state = self.modify()?;
		state.write_chunk(chunk)
	}

	/// Write a chunk of data to the frame.
	///
	/// Deprecated: use [`Self::write`] instead.
	#[deprecated(note = "use write(chunk) instead")]
	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		self.write(chunk)
	}

	/// Verify that all bytes have been written.
	///
	/// Returns [Error::WrongSize] if the bytes written don't match [Frame::size].
	pub fn finish(&mut self) -> Result<()> {
		let state = self.modify()?;
		if state.remaining != 0 {
			return Err(Error::WrongSize);
		}
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
			index: 0,
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
}

impl Clone for FrameProducer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			state: self.state.clone(),
		}
	}
}

impl From<Frame> for FrameProducer {
	fn from(info: Frame) -> Self {
		FrameProducer::new(info)
	}
}

impl std::ops::Deref for FrameConsumer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// Used to consume a frame's worth of data in chunks.
#[derive(Clone)]
pub struct FrameConsumer {
	// Immutable stream state.
	info: Frame,

	// Shared state with the producer.
	state: conducer::Consumer<FrameState>,

	// The number of chunks we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl FrameConsumer {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&conducer::Ref<'_, FrameState>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for all remaining data without blocking.
	pub fn poll_read_all(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Bytes>> {
		let data = ready!(self.poll(waiter, |state| state.poll_read_all(self.index))?);
		self.index = usize::MAX;
		Poll::Ready(Ok(data))
	}

	/// Return all of the remaining chunks concatenated together.
	pub async fn read_all(&mut self) -> Result<Bytes> {
		conducer::wait(|waiter| self.poll_read_all(waiter)).await
	}

	/// Return all of the remaining chunks of the frame, without blocking.
	pub fn poll_read_all_chunks(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Vec<Bytes>>> {
		let chunks = ready!(self.poll(waiter, |state| {
			// This is more complicated because we need to make a copy of the chunks while holding the lock..
			state
				.poll_read_all_chunks(self.index)
				.map(|res| res.map(|chunks| chunks.to_vec()))
		})?);
		self.index += chunks.len();

		Poll::Ready(Ok(chunks))
	}

	/// Poll for the next chunk, without blocking.
	pub fn poll_read_chunk(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Bytes>>> {
		let Some(chunk) = ready!(self.poll(waiter, |state| state.poll_read_chunk(self.index))?) else {
			return Poll::Ready(Ok(None));
		};
		self.index += 1;
		Poll::Ready(Ok(Some(chunk)))
	}

	/// Return the next chunk.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_read_chunk(waiter)).await
	}

	/// Poll for the next chunks, without blocking.
	pub fn poll_read_chunks(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Vec<Bytes>>> {
		let chunks = ready!(self.poll(waiter, |state| state.poll_read_chunks(self.index))?);
		self.index += chunks.len();
		Poll::Ready(Ok(chunks))
	}

	/// Read all of the remaining chunks into a vector.
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
		producer.write(Bytes::from_static(b"world")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let c1 = consumer.read_chunk().now_or_never().unwrap().unwrap();
		assert_eq!(c1, Some(Bytes::from_static(b"hello")));
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
		assert_eq!(chunks.len(), 2);
		assert_eq!(chunks[0], Bytes::from_static(b"hello"));
		assert_eq!(chunks[1], Bytes::from_static(b"world"));
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
}
