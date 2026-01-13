use std::ops::Deref;

use bytes::{Bytes, BytesMut};

use super::{Consumer, Produce, Producer};
use crate::{Error, Result, Time};

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
	pub instant: Time,

	/// The size of the frame in bytes.
	pub size: usize,
}

impl Frame {
	/// A helper to create a frame with the current time.
	pub fn new(size: usize) -> Self {
		Self {
			instant: Time::now(),
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
			instant: Time::now(),
			size,
		}
	}
}

impl From<u64> for Frame {
	fn from(size: u64) -> Self {
		Self {
			instant: Time::now(),
			size: size as usize,
		}
	}
}

impl From<u32> for Frame {
	fn from(size: u32) -> Self {
		Self {
			instant: Time::now(),
			size: size as usize,
		}
	}
}

impl From<u16> for Frame {
	fn from(size: u16) -> Self {
		Self {
			instant: Time::now(),
			size: size as usize,
		}
	}
}

#[derive(Default, Debug)]
struct FrameState {
	// The chunks that has been written thus far
	chunks: Vec<Bytes>,

	// Sanity check to ensure we don't write more than the frame size.
	remaining: usize,
}

impl FrameState {
	fn new(size: usize) -> Self {
		Self {
			chunks: Vec::new(),
			remaining: size,
		}
	}

	fn write_chunk(&mut self, chunk: Bytes) -> Result<()> {
		self.remaining = self.remaining.checked_sub(chunk.len()).ok_or(Error::WrongSize)?;
		self.chunks.push(chunk);
		Ok(())
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

	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		self.state.modify(|state| state.write_chunk(chunk.into()))?
	}

	pub fn close(&mut self) -> Result<()> {
		if self.state.borrow().remaining != 0 {
			return Err(Error::WrongSize);
		}
		self.state.close()
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		self.state.abort(err)
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
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		let state = self
			.state
			.wait_for(|state| state.chunks.get(self.index).is_some())
			.await?;

		if let Some(chunk) = state.chunks.get(self.index).cloned() {
			self.index += 1;
			Ok(Some(chunk))
		} else {
			Ok(None)
		}
	}

	/// Read all of the remaining chunks into a vector.
	pub async fn read_chunks(&mut self) -> Result<Vec<Bytes>> {
		// Wait until the writer is done before even attempting to read.
		// That way this function can be cancelled without consuming half of the frame.
		self.state.closed().await?;

		// Get all of the remaining chunks.
		let state = self.state.borrow();
		let chunks = state.chunks[self.index..].to_vec();
		self.index = state.chunks.len();

		Ok(chunks)
	}

	/// Return all of the remaining chunks concatenated together.
	pub async fn read_all(&mut self) -> Result<Bytes> {
		// Wait until the writer is done before even attempting to read.
		// That way this function can be cancelled without consuming half of the frame.
		self.state.closed().await?;

		// Get all of the remaining chunks.
		let state = self.state.borrow();
		let chunks = &state.chunks[self.index..];
		self.index = state.chunks.len();

		// We know the final size so we can allocate the buffer upfront.
		let size = chunks.iter().map(Bytes::len).sum();

		// We know the final size so we can allocate the buffer upfront.
		let mut buf = BytesMut::with_capacity(size);

		// Copy the chunks into the buffer.
		for chunk in chunks {
			buf.extend_from_slice(chunk);
		}

		Ok(buf.freeze())
	}
}

impl Deref for FrameConsumer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}
