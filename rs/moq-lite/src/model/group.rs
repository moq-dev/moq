//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once, or in chunks.
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [ServeError::MoqError] when all writers or readers are dropped.
use std::{fmt, future::Future, ops::Deref};

use bytes::Bytes;
use futures::FutureExt;
use tokio::sync::watch;

use crate::{Error, Result};

use super::{Frame, FrameConsumer, FrameProducer};

#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Group {
	pub sequence: u64,
}

impl<T: Into<u64>> From<T> for Group {
	fn from(sequence: T) -> Self {
		Self {
			sequence: sequence.into(),
		}
	}
}

#[derive(Default, Debug)]
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

	info: Group,
}

impl fmt::Debug for GroupProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("GroupProducer")
			.field("info", &self.info)
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl GroupProducer {
	pub fn new(info: Group) -> Self {
		Self {
			info,
			state: Default::default(),
		}
	}

	pub fn info(&self) -> &Group {
		&self.info
	}

	/// A helper method to write a frame from a single byte buffer.
	///
	/// If you want to write multiple chunks, use [Self::create] or [Self::append].
	/// But an upfront size is required.
	pub fn write_frame<B: Into<Bytes>>(&mut self, frame: B) -> Result<()> {
		let data = frame.into();
		let frame = Frame {
			size: data.len() as u64,
		};
		let mut frame = self.create_frame(frame)?;
		frame.write_chunk(data)?;
		frame.close()?;

		Ok(())
	}

	/// Create a frame with an upfront size
	pub fn create_frame(&mut self, info: Frame) -> Result<FrameProducer> {
		let frame = FrameProducer::new(info);
		self.append_frame(frame.consume())?;
		Ok(frame)
	}

	/// Append a frame to the group.
	pub fn append_frame(&mut self, frame: FrameConsumer) -> Result<()> {
		let mut result = Ok(());

		tracing::trace!(group = %self.info.sequence, "appending frame");
		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			state.frames.push(frame);
			true
		});
		tracing::trace!(group = %self.info.sequence, ?result, "appended frame");

		result
	}

	// Clean termination of the group.
	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		tracing::trace!(group = %self.info.sequence, "closing group");
		self.state.send_if_modified(|state| {
			if let Some(closed) = state.closed.clone() {
				result = Err(closed.err().unwrap_or(Error::Cancel));
				return false;
			}

			state.closed = Some(Ok(()));
			true
		});
		tracing::trace!(group = %self.info.sequence, ?result, "closed group");

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		tracing::trace!(group = %self.info.sequence, "aborting group");
		self.state.send_if_modified(|state| {
			if let Some(Err(closed)) = state.closed.clone() {
				result = Err(closed);
				return false;
			}

			state.closed = Some(Err(err));
			true
		});
		tracing::trace!(group = %self.info.sequence, ?result, "aborted group");

		result
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

	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
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

impl Deref for GroupProducer {
	type Target = Group;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

/// Consume a group, frame-by-frame.
///
/// If the consumer is cloned, it will receive a copy of all unread frames.
#[derive(Clone)]
pub struct GroupConsumer {
	// Modify the stream state.
	state: watch::Receiver<GroupState>,

	// Immutable stream state.
	info: Group,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,

	// Used to make read_frame cancel safe.
	active: Option<FrameConsumer>,
}

impl fmt::Debug for GroupConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("GroupConsumer")
			.field("info", &self.info)
			.field("state", &self.state.borrow().deref())
			.field("index", &self.index)
			.finish()
	}
}

impl GroupConsumer {
	pub fn info(&self) -> &Group {
		&self.info
	}

	/// Read the next frame.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>> {
		// In order to be cancel safe, we need to save the active frame.
		// That way if this method gets cancelled, we can resume where we left off.
		if self.active.is_none() {
			self.active = self.next_frame().await?;
		};

		// Read the frame in one go, which is cancel safe.
		let frame = match self.active.as_mut() {
			Some(frame) => frame.read_all().await?,
			None => return Ok(None),
		};

		self.active = None;

		Ok(Some(frame))
	}

	/// Return a reader for the next frame.
	pub async fn next_frame(&mut self) -> Result<Option<FrameConsumer>> {
		tracing::trace!(group = %self.info.sequence, "waiting for frame");

		// Just in case someone called read_frame, cancelled it, then called next_frame.
		if let Some(frame) = self.active.take() {
			tracing::trace!("using active");
			return Ok(Some(frame));
		}

		let state = self
			.state
			.wait_for(|state| self.index < state.frames.len() || state.closed.is_some())
			.await
			.map_err(|_| Error::Cancel)?;

		if let Some(frame) = state.frames.get(self.index).cloned() {
			tracing::trace!(group = %self.info.sequence, index = %self.index, "got frame");
			self.index += 1;
			return Ok(Some(frame.clone()));
		}

		let closed = state.closed.clone().expect("wait_for returned");
		tracing::trace!(group = %self.info.sequence, ?closed, "got closed");
		closed.map(|_| None)
	}

	/// Proxy all frames and errors to the given producer.
	///
	/// Returns an error on any unexpected close, which can happen if the [GroupProducer] is cloned.
	pub(super) async fn proxy(mut self, mut dst: GroupProducer) -> Result<()> {
		while let Some(frame) = self.next_frame().await.transpose() {
			match frame {
				Ok(frame) => {
					let dst = dst.create_frame(frame.info().clone())?;
					web_async::spawn_named("proxy-frame", frame.proxy(dst).map(|_| ()));
				}
				Err(err) => return dst.abort(err),
			}
		}

		// Close the group.
		dst.close()
	}

	pub async fn closed(&self) -> Result<()> {
		match self.state.clone().wait_for(|state| state.closed.is_some()).await {
			Ok(state) => state.closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}
}

impl Deref for GroupConsumer {
	type Target = Group;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}
