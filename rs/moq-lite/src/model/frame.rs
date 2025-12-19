use std::{future::Future, ops::Deref};

use bytes::{Bytes, BytesMut};
use tokio::sync::watch;

use crate::{Error, Result};

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frame {
	pub size: u64,
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
	info: Frame,

	// Mutable stream state.
	state: watch::Sender<FrameState>,

	// Sanity check to ensure we don't write more than the frame size.
	written: usize,
}

impl FrameProducer {
	pub fn new(info: Frame) -> Self {
		Self {
			info,
			state: Default::default(),
			written: 0,
		}
	}

	pub fn info(&self) -> &Frame {
		&self.info
	}

	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		let chunk = chunk.into();
		if self.written + chunk.len() > self.info.size as usize {
			return Err(Error::WrongSize);
		}

		self.written += chunk.len();

		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			state.chunks.push(chunk);
			true
		});

		result
	}

	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		if self.written != self.info.size as usize {
			return Err(Error::WrongSize);
		}

		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			state.closed = Some(Ok(()));
			true
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			if let Some(Err(closed)) = state.closed.clone() {
				result = Err(closed);
				return false;
			}

			state.closed = Some(Err(err));
			true
		});

		result
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

impl Deref for FrameProducer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// Used to consume a frame's worth of data in chunks.
///
/// If the consumer is cloned, it will receive a copy of all unread chunks.
#[derive(Clone)]
pub struct FrameConsumer {
	// Immutable stream state.
	info: Frame,

	// Modify the stream state.
	state: watch::Receiver<FrameState>,

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

	/// Proxy all chunks and errors to the given producer.
	///
	/// Returns an error on any unexpected close, which can happen if the [GroupProducer] is cloned.
	pub async fn proxy(mut self, mut dst: FrameProducer) -> Result<()> {
		loop {
			match self.read_chunk().await {
				Ok(Some(chunk)) => dst.write_chunk(chunk)?,
				Ok(None) => return dst.close(),
				Err(err) => return dst.abort(err),
			}
		}
	}
}

impl Deref for FrameConsumer {
	type Target = Frame;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}
