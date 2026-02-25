use std::task::Poll;

use bytes::{Bytes, BytesMut};

use crate::{Error, Result};

use super::state::{Consumer, Producer};
use super::waiter::waiter_fn;

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
}

impl FrameState {
	fn write_chunk(&mut self, chunk: Bytes) -> Result<()> {
		self.remaining = self.remaining.checked_sub(chunk.len() as u64).ok_or(Error::WrongSize)?;
		self.chunks.push(chunk);
		Ok(())
	}

	fn poll_read_chunk(&self, index: usize) -> Poll<Option<Bytes>> {
		if let Some(chunk) = self.chunks.get(index).cloned() {
			Poll::Ready(Some(chunk))
		} else if self.remaining == 0 {
			Poll::Ready(None)
		} else {
			Poll::Pending
		}
	}

	fn poll_read_chunks(&self, index: usize) -> Poll<Vec<Bytes>> {
		if index >= self.chunks.len() && self.remaining == 0 {
			return Poll::Ready(Vec::new());
		}
		if self.remaining == 0 {
			Poll::Ready(self.chunks[index..].to_vec())
		} else {
			Poll::Pending
		}
	}

	fn poll_read_all(&self, index: usize) -> Poll<Bytes> {
		if self.remaining > 0 {
			return Poll::Pending;
		}

		if index >= self.chunks.len() {
			return Poll::Ready(Bytes::new());
		}

		let chunks = &self.chunks[index..];
		let size = chunks.iter().map(Bytes::len).sum();
		let mut buf = BytesMut::with_capacity(size);
		for chunk in chunks {
			buf.extend_from_slice(chunk);
		}
		Poll::Ready(buf.freeze())
	}
}

/// Used to write a frame's worth of data in chunks.
pub struct FrameProducer {
	// Immutable stream state.
	pub info: Frame,

	// Mutable stream state.
	state: Producer<FrameState>,
}

impl FrameProducer {
	pub fn new(info: Frame) -> Self {
		let state = FrameState {
			chunks: Vec::new(),
			remaining: info.size,
		};
		Self {
			info,
			state: Producer::new(state),
		}
	}

	pub fn write_chunk<B: Into<Bytes>>(&mut self, chunk: B) -> Result<()> {
		let chunk = chunk.into();
		let mut state = self.state.modify()?;
		state.write_chunk(chunk)
	}

	/// Optional: mark the frame as finished when all bytes have been written.
	pub fn finish(&mut self) -> Result<()> {
		let state = self.state.modify()?;
		if state.remaining != 0 {
			return Err(Error::WrongSize);
		}
		Ok(())
	}

	pub fn close(&mut self, err: Error) -> Result<()> {
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

	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await
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

/// Used to consume a frame's worth of data in chunks.
#[derive(Clone)]
pub struct FrameConsumer {
	// Immutable stream state.
	pub info: Frame,

	// Shared state with the producer.
	state: Consumer<FrameState>,

	// The number of chunks we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl FrameConsumer {
	/// Return the next chunk.
	pub async fn read_chunk(&mut self) -> Result<Option<Bytes>> {
		let index = self.index;
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_read_chunk(index))).await?;
		if res.is_some() {
			self.index += 1;
		}
		Ok(res)
	}

	/// Read all of the remaining chunks into a vector.
	/// Cancel-safe: returns all or nothing.
	pub async fn read_chunks(&mut self) -> Result<Vec<Bytes>> {
		let index = self.index;
		let chunks = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_read_chunks(index))).await?;
		self.index += chunks.len();
		Ok(chunks)
	}

	/// Return all of the remaining chunks concatenated together.
	/// Cancel-safe: returns all or nothing.
	pub async fn read_all(&mut self) -> Result<Bytes> {
		let index = self.index;
		let data = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_read_all(index))).await?;
		self.index = usize::MAX; // consumed everything
		Ok(data)
	}
}
