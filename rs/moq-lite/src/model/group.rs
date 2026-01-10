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
use tokio::sync::watch;

use crate::{Error, ExpiresConsumer, ExpiresProducer, Result, Time};

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
	frames: Vec<FrameProducer>,

	// Whether the group is closed
	closed: Option<Result<()>>,

	// The maximum instant of the frames in the group.
	// TODO prevent going backwards instead?
	max_instant: Option<Time>,
}

impl GroupState {
	fn append_frame(&mut self, frame: FrameProducer) -> Result<()> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		self.max_instant = Some(self.max_instant.unwrap_or_default().max(frame.instant));

		self.frames.push(frame);

		Ok(())
	}

	fn close(&mut self) -> Result<()> {
		if let Some(closed) = self.closed.clone() {
			return Err(closed.err().unwrap_or(Error::Cancel));
		}

		self.closed = Some(Ok(()));

		Ok(())
	}

	fn abort(&mut self, err: Error) -> Result<()> {
		if let Some(Err(err)) = self.closed.clone() {
			return Err(err);
		}

		self.closed = Some(Err(err));

		Ok(())
	}
}

/// Create a group, frame-by-frame.
#[derive(Clone)]
pub struct GroupProducer {
	// Mutable stream state.
	state: watch::Sender<GroupState>,
	info: Group,
	expires: ExpiresProducer,
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
	pub fn new(info: Group, expires: ExpiresProducer) -> Self {
		Self {
			info,
			state: Default::default(),
			expires,
		}
	}

	pub fn info(&self) -> &Group {
		&self.info
	}

	/// A helper method to write a frame from a single byte buffer.
	///
	/// If you want to write multiple chunks, use [Self::create_frame] or [Self::append_frame].
	pub fn write_frame<B: Into<Bytes>>(&mut self, frame: B, instant: Time) -> Result<()> {
		let data = frame.into();
		let frame = Frame {
			size: data.len(),
			instant,
		};
		let mut frame = self.create_frame(frame)?;
		frame.write_chunk(data)?;
		frame.close()?;

		Ok(())
	}

	/// Create a frame with an upfront size
	pub fn create_frame(&mut self, info: Frame) -> Result<FrameProducer> {
		let frame = FrameProducer::new(info);
		self.append_frame(frame.clone())?;
		Ok(frame)
	}

	/// Append a frame to the group.
	pub fn append_frame(&mut self, frame: FrameProducer) -> Result<()> {
		let mut result = Ok(());

		// Add the current frame to the expiration tracker.
		// NOTE: This might return an error if the current group is expired.
		self.expires.create_frame(self.info.sequence, frame.instant)?;

		self.state.send_if_modified(|state| {
			result = state.append_frame(frame);
			result.is_ok()
		});

		result
	}

	// Clean termination of the group.
	pub fn close(&mut self) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			result = state.close();
			result.is_ok()
		});

		result
	}

	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut result = Ok(());

		self.state.send_if_modified(|state| {
			result = state.abort(err);
			result.is_ok()
		});

		result
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> GroupConsumer {
		GroupConsumer {
			info: self.info.clone(),
			state: self.state.subscribe(),
			index: 0,
			active: None,
			expires: self.expires.consume(),
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

	// Used to check if the group is expired early.
	expires: ExpiresConsumer,
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
		// Just in case someone called read_frame, cancelled it, then called next_frame.
		if let Some(frame) = self.active.take() {
			return Ok(Some(frame));
		}

		let max_instant = self.state.borrow().max_instant;

		let state = tokio::select! {
			// Wait until a new frame.
			state = self.state.wait_for(|state| self.index < state.frames.len() || state.closed.is_some()) => state,
			// Or wait until the maximum instant in the group is expired.
			err = self.expires.wait_expired(self.info.sequence, max_instant.unwrap_or_default()), if max_instant.is_some() => return Err(err),
			// NOTE: We don't have to wait for a new maximum, because it will satisfy the wait for the next frame.

		};

		let state = state.map_err(|_| Error::Cancel)?;

		if let Some(frame) = state.frames.get(self.index).cloned() {
			self.index += 1;
			return Ok(Some(frame.consume()));
		}

		let closed = state.closed.clone().expect("wait_for returned");
		closed.map(|_| None)
	}

	pub async fn closed(&self) -> Result<()> {
		match self.state.clone().wait_for(|state| state.closed.is_some()).await {
			Ok(state) => state.closed.clone().unwrap(),
			Err(_) => Err(Error::Cancel),
		}
	}

	/// Blocks until the first instant of a frame in the group has arrived.
	pub async fn instant(&self) -> Result<Time> {
		self.state
			.clone()
			.wait_for(|state| !state.frames.is_empty())
			.await
			.map_err(|_| Error::Cancel)?;
		Ok(self.state.borrow().frames[0].instant)
	}
}

impl Deref for GroupConsumer {
	type Target = Group;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}
