//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once, or in chunks.
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [ServeError::MoqError] when all writers or readers are dropped.
use std::future::Future;

use bytes::Bytes;
use tokio::sync::watch;

use crate::{Error, Result};

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
	// The frames that has been written thus far
	frames: Vec<FrameConsumer>,

	// Whether the group is closed
	closed: Option<Result<()>>,
}

/// Create a group, frame-by-frame.
#[derive(Clone)]
pub struct GroupProducer {
	// Mutable stream state.
	state: watch::Sender<GroupState>,

	// Immutable stream state.
	pub info: Group,
}

impl GroupProducer {
	pub fn new(info: Group) -> Self {
		Self {
			info,
			state: Default::default(),
		}
	}

	/// A helper method to write a frame from a single byte buffer.
	///
	/// If you want to write multiple chunks, use [Self::create_frame] to get a frame producer.
	/// But an upfront size is required.
	pub fn write_frame<B: Into<Bytes>>(&mut self, frame: B) {
		let data = frame.into();
		let frame = Frame {
			size: data.len() as u64,
		};
		let mut frame = self.create_frame(frame);
		frame.write_chunk(data);
		frame.close();
	}

	/// Create a frame with an upfront size
	pub fn create_frame(&mut self, info: Frame) -> FrameProducer {
		let frame = info.produce();
		self.append_frame(frame.consume());
		frame
	}

	/// Append a frame to the group.
	pub fn append_frame(&mut self, consumer: FrameConsumer) {
		self.state.send_modify(|state| {
			assert!(state.closed.is_none());
			state.frames.push(consumer)
		});
	}

	// Clean termination of the group.
	pub fn close(self) {
		self.state.send_modify(|state| state.closed = Some(Ok(())));
	}

	pub fn abort(self, err: Error) {
		self.state.send_modify(|state| state.closed = Some(Err(err)));
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> GroupConsumer {
		GroupConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			index: 0,
			active: None,
		}
	}

	pub fn unused(&self) -> impl Future<Output = ()> + use<> {
		let state = self.state.clone();
		async move {
			state.closed().await;
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
	// Modify the stream state.
	state: watch::Receiver<GroupState>,

	// Immutable stream state.
	pub info: Group,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,

	// Used to make read_frame cancel safe.
	active: Option<FrameConsumer>,
}

impl GroupConsumer {
	/// Read the next frame.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>> {
		// In order to be cancel safe, we need to save the active frame.
		// That way if this method gets cancelled, we can resume where we left off.
		if self.active.is_none() {
			self.active = self.next_frame().await?;
		};

		// Read the frame in one go, which is cancel safe.
		let Some(frame) = self.active.as_mut() else {
			return Ok(None);
		};
		let frame = frame.read_all().await?;

		self.active = None;

		Ok(Some(frame))
	}

	/// Return a reader for the next frame.
	pub async fn next_frame(&mut self) -> Result<Option<FrameConsumer>> {
		// Just in case someone called read_frame, cancelled it, then called next_frame.
		if let Some(frame) = self.active.take() {
			return Ok(Some(frame));
		}

		loop {
			{
				let state = self.state.borrow_and_update();

				if let Some(frame) = state.frames.get(self.index).cloned() {
					self.index += 1;
					return Ok(Some(frame));
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

	pub async fn get_frame(&self, index: usize) -> Result<Option<FrameConsumer>> {
		let mut state = self.state.clone();
		let Ok(state) = state
			.wait_for(|state| index < state.frames.len() || state.closed.is_some())
			.await
		else {
			return Err(Error::Cancel);
		};

		if let Some(frame) = state.frames.get(index).cloned() {
			return Ok(Some(frame));
		}

		match &state.closed {
			Some(Ok(_)) => Ok(None),
			Some(Err(err)) => Err(err.clone()),
			_ => unreachable!(),
		}
	}
}
