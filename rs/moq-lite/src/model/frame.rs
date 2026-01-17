use std::{ops::Deref, task::Poll};

use bytes::{Bytes, BytesMut};

use super::{Consumer, Produce, Producer};
use crate::{
	model::waiter::{waiter_fn, Waiter},
	Error, Time,
};

/// A unit of data, representing a point in time.
///
/// This is often a video frame or a packet of audio samples.
/// The instant is when the frame was captured and should be rendered, scoped to the *track*.
///
/// The size must be known upfront. If you don't know the size, write a Frame for each chunk.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frame {
	/// A timestamp (in milliseconds) when the frame was originally created, scoped to the *track*.
	///
	/// This *should* be set as early as possible in the pipeline and proxied through all relays.
	/// There is no clock synchronization or "zero" value; everything is relative to the track.
	///
	/// This may be used by the application as a replacement for "presentation timestamp", even across tracks.
	/// However, the lack of granularity and inability to go backwards limits its usefulness.
	pub timestamp: Time,

	/// The size of the frame in bytes.
	pub size: usize,
}

impl Frame {
	/// A helper to create a frame with the current time.
	pub fn new(size: usize) -> Self {
		Self {
			timestamp: Time::now(),
			size,
		}
	}

	/// Create a new producer and consumer for the frame.
	pub fn produce(self) -> Produce<FrameProducer, FrameConsumer> {
		let producer = FrameProducer::new(self);
		let consumer = producer.consume();
		Produce { producer, consumer }
	}
}

impl From<usize> for Frame {
	fn from(size: usize) -> Self {
		Self {
			timestamp: Time::now(),
			size,
		}
	}
}

#[derive(Default, Debug)]
struct FrameState {
	// The chunks that has been written thus far
	chunks: Vec<Bytes>,

	// The remaining size to write.
	remaining: usize,
}

impl FrameState {
	fn new(size: usize) -> Self {
		Self {
			chunks: Vec::new(),
			remaining: size,
		}
	}

	fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Error> {
		self.remaining = self.remaining.checked_sub(chunk.len()).ok_or(Error::WrongSize)?;
		self.chunks.push(chunk);
		Ok(())
	}

	fn read_chunk(&self, index: &mut usize) -> Poll<Option<Bytes>> {
		if let Some(chunk) = self.chunks.get(*index).cloned() {
			*index += 1;
			return Poll::Ready(Some(chunk));
		}

		if self.remaining == 0 {
			return Poll::Ready(None);
		}

		Poll::Pending
	}

	// Read all of the remaining chunks into a vector.
	fn read_chunks(&self, index: &mut usize) -> Poll<Vec<Bytes>> {
		if self.remaining != 0 {
			return Poll::Pending;
		}

		// Get all of the remaining chunks.
		let chunks = &self.chunks[*index..];
		*index = self.chunks.len();

		Poll::Ready(chunks.to_vec())
	}

	fn read_all(&self, index: &mut usize) -> Poll<Bytes> {
		if self.remaining != 0 {
			return Poll::Pending;
		}

		let chunks = &self.chunks[*index..];
		*index = self.chunks.len();

		// We know the final size so we can allocate the buffer upfront.
		let size = chunks.iter().map(Bytes::len).sum();

		// We know the final size so we can allocate the buffer upfront.
		let mut buf = BytesMut::with_capacity(size);

		// Copy the chunks into the buffer.
		for chunk in chunks {
			buf.extend_from_slice(chunk);
		}

		Poll::Ready(buf.freeze())
	}
}

/// Used to write a frame's worth of data in chunks.
#[derive(Clone, Debug)]
pub struct FrameProducer {
	info: Frame,

	// Mutable stream state.
	state: Producer<FrameState>,
}

impl FrameProducer {
	pub fn new(info: Frame) -> Self {
		Self {
			state: Producer::new(FrameState::new(info.size)),
			info,
		}
	}

	pub fn info(&self) -> &Frame {
		&self.info
	}

	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) -> Result<(), Error> {
		self.state.modify()?.write_chunk(chunk.into())
	}

	/// Optional: Sanity check to ensure that all data has been written.
	pub fn final_chunk(&mut self) -> Result<(), Error> {
		if self.state.borrow().remaining != 0 {
			return Err(Error::WrongSize);
		}
		Ok(())
	}

	pub fn abort(self, err: Error) -> Result<(), Error> {
		self.state.close(err)
	}

	/// Create a new consumer for the frame.
	pub fn consume(&self) -> FrameConsumer {
		FrameConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
		}
	}
}

impl From<Frame> for FrameProducer {
	fn from(info: Frame) -> Self {
		FrameProducer::new(info)
	}
}

impl Deref for FrameProducer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// Used to consume a frame's worth of data in chunks.
///
/// If the consumer is cloned, it will receive a copy of all unread chunks.
#[derive(Clone, Debug)]
pub struct FrameConsumer {
	// Immutable stream state.
	info: Frame,

	// Modify the stream state.
	state: Consumer<FrameState>,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl FrameConsumer {
	pub fn info(&self) -> &Frame {
		&self.info
	}

	/// Return the next chunk.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>, Error> {
		waiter_fn(move |waiter| self.poll_read_chunk(waiter)).await
	}

	pub fn poll_read_chunk(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Option<Bytes>, Error>> {
		self.state.poll(waiter, |state| state.read_chunk(&mut self.index))
	}

	/// Read all of the remaining chunks into a vector.
	pub async fn read_chunks(&mut self) -> Result<Vec<Bytes>, Error> {
		waiter_fn(move |waiter| self.poll_read_chunks(waiter)).await
	}

	pub fn poll_read_chunks(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Vec<Bytes>, Error>> {
		self.state.poll(waiter, |state| state.read_chunks(&mut self.index))
	}

	/// Return all of the remaining chunks concatenated together.
	pub async fn read_all(&mut self) -> Result<Bytes, Error> {
		waiter_fn(move |waiter| self.poll_read_all(waiter)).await
	}

	pub fn poll_read_all(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Bytes, Error>> {
		self.state.poll(waiter, |state| state.read_all(&mut self.index))
	}
}

impl Deref for FrameConsumer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_frame_new() {
		let frame = Frame::new(100);
		assert_eq!(frame.size, 100);
		assert!(frame.timestamp > Time::ZERO);
	}

	#[test]
	fn test_frame_from_usize() {
		let frame: Frame = 100usize.into();
		assert_eq!(frame.size, 100);
	}

	#[tokio::test]
	async fn test_frame_write_read_single_chunk() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 10,
		};

		let mut producer = FrameProducer::new(frame.clone());
		let mut consumer = producer.consume();

		// Write data
		producer.write_chunk(Bytes::from("hello world")).unwrap_err(); // Too big
		producer.write_chunk(Bytes::from("hello")).unwrap();
		producer.write_chunk(Bytes::from("world")).unwrap();
		producer.final_chunk().unwrap();

		// Read data
		let chunk1 = consumer.read_chunk().await.unwrap().unwrap();
		assert_eq!(chunk1, Bytes::from("hello"));

		let chunk2 = consumer.read_chunk().await.unwrap().unwrap();
		assert_eq!(chunk2, Bytes::from("world"));

		// No more chunks
		let chunk3 = consumer.read_chunk().await.unwrap();
		assert!(chunk3.is_none());
	}

	#[tokio::test]
	async fn test_frame_read_all() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 10,
		};

		let mut producer = FrameProducer::new(frame);
		let mut consumer = producer.consume();

		producer.write_chunk(Bytes::from("hello")).unwrap();
		producer.write_chunk(Bytes::from("world")).unwrap();
		producer.final_chunk().unwrap();

		let all = consumer.read_all().await.unwrap();
		assert_eq!(all, Bytes::from("helloworld"));
	}

	#[tokio::test]
	async fn test_frame_read_chunks() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 10,
		};

		let mut producer = FrameProducer::new(frame);
		let mut consumer = producer.consume();

		producer.write_chunk(Bytes::from("hello")).unwrap();
		producer.write_chunk(Bytes::from("world")).unwrap();
		producer.final_chunk().unwrap();

		let chunks = consumer.read_chunks().await.unwrap();
		assert_eq!(chunks.len(), 2);
		assert_eq!(chunks[0], Bytes::from("hello"));
		assert_eq!(chunks[1], Bytes::from("world"));
	}

	#[tokio::test]
	async fn test_frame_wrong_size_too_large() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 5,
		};

		let mut producer = FrameProducer::new(frame);
		producer.write_chunk(Bytes::from("hello")).unwrap();

		// Try to write more than the size
		let result = producer.write_chunk(Bytes::from("x"));
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn test_frame_wrong_size_too_small() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 10,
		};

		let mut producer = FrameProducer::new(frame);
		producer.write_chunk(Bytes::from("hello")).unwrap();

		// Try to close before writing all data
		let result = producer.final_chunk();
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn test_frame_multiple_consumers() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 10,
		};

		let mut producer = FrameProducer::new(frame);
		let mut consumer1 = producer.consume();
		let mut consumer2 = producer.consume();

		producer.write_chunk(Bytes::from("hello")).unwrap();
		producer.write_chunk(Bytes::from("world")).unwrap();
		producer.final_chunk().unwrap();

		// Both consumers should get all data
		let data1 = consumer1.read_all().await.unwrap();
		let data2 = consumer2.read_all().await.unwrap();
		assert_eq!(data1, Bytes::from("helloworld"));
		assert_eq!(data2, Bytes::from("helloworld"));
	}

	#[tokio::test]
	async fn test_frame_abort() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 10,
		};

		let mut producer = FrameProducer::new(frame);
		let mut consumer = producer.consume();

		producer.write_chunk(Bytes::from("hello")).unwrap();
		producer.abort(Error::Cancel).unwrap();

		// Consumer should get an error
		let result = consumer.read_all().await;
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn test_frame_produce_helper() {
		let frame = Frame {
			timestamp: Time::from_millis(100).unwrap(),
			size: 5,
		};

		let mut pair = frame.produce();
		pair.producer.write_chunk(Bytes::from("hello")).unwrap();
		pair.producer.final_chunk().unwrap();

		let data = pair.consumer.read_all().await.unwrap();
		assert_eq!(data, Bytes::from("hello"));
	}
}
