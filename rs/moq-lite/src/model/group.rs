//! A group is a stream of frames, split into a [GroupProducer] and [GroupConsumer] handle.
//!
//! A [GroupProducer] writes an ordered stream of frames.
//! Frames can be written all at once, or in chunks.
//!
//! A [GroupConsumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [Error] when all writers or readers are dropped.
use std::task::Poll;

use bytes::Bytes;

use crate::{Error, Result};

use super::state::{Consumer, Producer};
use super::waiter::waiter_fn;
use super::{Frame, FrameConsumer, FrameProducer};

/// A group contains a sequence number because they can arrive out of order.
///
/// You can use [crate::TrackProducer::append_group] if you just want to +1 the sequence number.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Group {
	pub sequence: u64,
}

impl Group {
	pub fn produce(self) -> GroupProducer {
		GroupProducer::new(self)
	}
}

impl From<usize> for Group {
	fn from(sequence: usize) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

impl From<u64> for Group {
	fn from(sequence: u64) -> Self {
		Self { sequence }
	}
}

impl From<u32> for Group {
	fn from(sequence: u32) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

impl From<u16> for Group {
	fn from(sequence: u16) -> Self {
		Self {
			sequence: sequence as u64,
		}
	}
}

#[derive(Default)]
struct GroupState {
	// The frames that have been written thus far.
	// We store producers so consumers can be created on-demand.
	frames: Vec<FrameProducer>,

	// Whether the group has been finalized (no more frames).
	fin: bool,
}

impl GroupState {
	fn poll_next_frame(&self, index: usize) -> Poll<Option<FrameProducer>> {
		if let Some(frame) = self.frames.get(index) {
			Poll::Ready(Some(frame.clone()))
		} else if self.fin {
			Poll::Ready(None)
		} else {
			Poll::Pending
		}
	}
}

/// Create a group, frame-by-frame.
pub struct GroupProducer {
	// Mutable stream state.
	state: Producer<GroupState>,

	// Immutable stream state.
	pub info: Group,
}

impl GroupProducer {
	pub fn new(info: Group) -> Self {
		Self {
			info,
			state: Producer::default(),
		}
	}

	/// A helper method to write a frame from a single byte buffer.
	///
	/// If you want to write multiple chunks, use [Self::create_frame] to get a frame producer.
	/// But an upfront size is required.
	pub fn write_frame<B: Into<Bytes>>(&mut self, frame: B) -> Result<()> {
		let data = frame.into();
		let frame = Frame {
			size: data.len() as u64,
		};
		let mut frame = self.create_frame(frame)?;
		frame.write_chunk(data)?;
		frame.finish()?;
		Ok(())
	}

	/// Create a frame with an upfront size
	pub fn create_frame(&mut self, info: Frame) -> Result<FrameProducer> {
		let frame = info.produce();
		self.append_frame(frame.clone())?;
		Ok(frame)
	}

	/// Append a frame producer to the group.
	pub fn append_frame(&mut self, frame: FrameProducer) -> Result<()> {
		let mut state = self.state.modify()?;
		if state.fin {
			return Err(Error::Closed);
		}
		state.frames.push(frame);
		Ok(())
	}

	/// Clean termination of the group.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.state.modify()?;
		state.fin = true;
		Ok(())
	}

	/// Close the group with the given error.
	///
	/// No updates can be made after this point.
	pub fn close(&mut self, err: Error) -> Result<()> {
		let mut state = self.state.modify()?;

		// Abort all frames still in progress.
		for frame in state.frames.iter_mut() {
			// Ignore errors, we don't care if the frame was already closed.
			frame.close(err.clone()).ok();
		}

		state.close(err);
		Ok(())
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> GroupConsumer {
		GroupConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
		}
	}

	pub async fn unused(&self) -> Result<()> {
		self.state.unused().await
	}
}

impl Clone for GroupProducer {
	fn clone(&self) -> Self {
		Self {
			info: self.info.clone(),
			state: self.state.clone(),
		}
	}
}

impl From<Group> for GroupProducer {
	fn from(info: Group) -> Self {
		GroupProducer::new(info)
	}
}

/// Consume a group, frame-by-frame.
#[derive(Clone)]
pub struct GroupConsumer {
	// Shared state with the producer.
	state: Consumer<GroupState>,

	// Immutable stream state.
	pub info: Group,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl GroupConsumer {
	/// Read the next frame's data all at once.
	///
	/// Cancel-safe: if cancelled after obtaining the frame but before reading,
	/// we retry from the same index and create a fresh consumer.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>> {
		// Step 1: Get the next frame producer from the group state.
		let index = self.index;
		let frame = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_next_frame(index))).await?;

		let Some(frame) = frame else {
			return Ok(None);
		};

		// Step 2: Read all data from the frame via a temporary consumer.
		// Cancel-safe because read_all returns all or nothing.
		let mut consumer = frame.consume();
		let data = consumer.read_all().await?;

		self.index += 1;
		Ok(Some(data))
	}

	/// Block until the frame at the given index is available.
	///
	/// Returns None if the group is finished and the index is out of range.
	pub async fn get_frame(&self, index: usize) -> Result<Option<FrameConsumer>> {
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_next_frame(index))).await?;
		Ok(res.map(|producer| producer.consume()))
	}

	/// Return a consumer for the next frame for chunked reading.
	pub async fn next_frame(&mut self) -> Result<Option<FrameConsumer>> {
		let index = self.index;
		let res = waiter_fn(|waiter| self.state.poll(waiter, |state| state.poll_next_frame(index))).await?;
		let consumer = res.map(|producer| {
			self.index += 1;
			producer.consume()
		});
		Ok(consumer)
	}
}
