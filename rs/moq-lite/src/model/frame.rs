use std::future::Future;

use bytes::{Bytes, BytesMut};
use tokio::sync::watch;

use crate::{Error, Produce, Result};

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frame {
	pub size: u64,
}

impl Frame {
	pub fn produce(self) -> Produce<FrameProducer, FrameConsumer> {
		let producer = FrameProducer::new(self);
		let consumer = producer.consume();
		Produce { producer, consumer }
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

#[derive(Default)]
struct FrameState {
	// The chunks that has been written thus far
	chunks: Vec<Bytes>,

	// Set when the writer or all readers are dropped.
	closed: Option<Result<()>>,
}

/// Used to write a frame's worth of data in chunks.
#[derive(Clone)]
pub struct FrameProducer {
	// Immutable stream state.
	pub info: Frame,

	// Mutable stream state.
	state: watch::Sender<FrameState>,

	// Sanity check to ensure we don't write more than the frame size.
	written: usize,
}

impl FrameProducer {
	fn new(info: Frame) -> Self {
		Self {
			info,
			state: Default::default(),
			written: 0,
		}
	}

	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) {
		let chunk = chunk.into();
		self.written += chunk.len();
		assert!(self.written <= self.info.size as usize);

		self.state.send_modify(|state| {
			assert!(state.closed.is_none());
			state.chunks.push(chunk);
		});
	}

	pub fn close(self) {
		assert!(self.written == self.info.size as usize);
		self.state.send_modify(|state| state.closed = Some(Ok(())));
	}

	pub fn abort(self, err: Error) {
		self.state.send_modify(|state| state.closed = Some(Err(err)));
	}

	/// Create a new consumer for the frame.
	pub fn consume(&self) -> FrameConsumer {
		FrameConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			index: 0,
		}
	}

	// Returns a Future so &self is not borrowed during the future.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let state = self.state.clone();
		async move {
			state.closed().await;
		}
	}
}

impl From<Frame> for FrameProducer {
	fn from(info: Frame) -> Self {
		FrameProducer::new(info)
	}
}

/// Used to consume a frame's worth of data in chunks.
#[derive(Clone)]
pub struct FrameConsumer {
	// Immutable stream state.
	pub info: Frame,

	// Modify the stream state.
	state: watch::Receiver<FrameState>,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl FrameConsumer {
	/// Return the next chunk.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		loop {
			{
				let state = self.state.borrow_and_update();

				if let Some(chunk) = state.chunks.get(self.index).cloned() {
					self.index += 1;
					return Ok(Some(chunk));
				}

				match &state.closed {
					Some(Ok(_)) => return Ok(None),
					Some(Err(err)) => return Err(err.clone()),
					_ => {}
				}
			}

			if self.state.changed().await.is_err() {
				return Err(Error::Cancel);
			}
		}
	}

	/// Read all of the remaining chunks into a vector.
	pub async fn read_chunks(&mut self) -> Result<Vec<Bytes>> {
		// Wait until the writer is done before even attempting to read.
		// That way this function can be cancelled without consuming half of the frame.
		let state = match self.state.wait_for(|state| state.closed.is_some()).await {
			Ok(state) => {
				if let Some(Err(err)) = &state.closed {
					return Err(err.clone());
				}
				state
			}
			Err(_) => return Err(Error::Cancel),
		};

		// Get all of the remaining chunks.
		let chunks = state.chunks[self.index..].to_vec();
		self.index = state.chunks.len();

		Ok(chunks)
	}

	/// Return all of the remaining chunks concatenated together.
	pub async fn read_all(&mut self) -> Result<Bytes> {
		// Wait until the writer is done before even attempting to read.
		// That way this function can be cancelled without consuming half of the frame.
		let state = match self.state.wait_for(|state| state.closed.is_some()).await {
			Ok(state) => {
				if let Some(Err(err)) = &state.closed {
					return Err(err.clone());
				}
				state
			}
			Err(_) => return Err(Error::Cancel),
		};

		// Get all of the remaining chunks.
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
