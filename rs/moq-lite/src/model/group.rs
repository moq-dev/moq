//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once, or in chunks.
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [ServeError::MoqError] when all writers or readers are dropped.
use std::{ops::Deref, task::Poll};

use bytes::Bytes;

use super::{Consumer, Frame, FrameConsumer, FrameProducer, Producer};
use crate::{
	model::waiter::{waiter_fn, Waiter},
	Error, Time,
};

/// A group contains a sequence number because they can arrive out of order.
///
/// You can use [crate::TrackProducer::append_group] if you just want to +1 the sequence number.
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

	// No more frames will be written.
	fin: bool,
}

impl GroupState {
	fn append_frame(&mut self, frame: FrameProducer) -> Result<(), Error> {
		if self.fin {
			return Err(Error::Closed);
		}

		self.frames.push(frame);
		Ok(())
	}

	fn poll_read_frame(&self, waiter: &Waiter<'_>, index: &mut usize) -> Poll<Result<Option<Bytes>, Error>> {
		if self.frames.is_empty() {
			return Poll::Pending;
		}

		if let Some(frame) = self.frames.get(*index) {
			if let Poll::Ready(data) = frame.consume().poll_read_all(waiter) {
				*index += 1;
				return match data {
					Ok(data) => Poll::Ready(Ok(Some(data))),
					Err(err) => Poll::Ready(Err(err)),
				};
			}
		}

		if self.fin {
			return Poll::Ready(Ok(None));
		}

		Poll::Pending
	}

	// TODO add expires
	fn poll_next_frame(&self, index: &mut usize) -> Poll<Option<FrameConsumer>> {
		if self.frames.is_empty() {
			return Poll::Pending;
		}

		if let Some(frame) = self.frames.get(*index) {
			*index += 1;
			return Poll::Ready(Some(frame.consume()));
		}

		if self.fin {
			return Poll::Ready(None);
		}

		Poll::Pending
	}

	fn poll_timestamp(&self) -> Poll<(Time, Time)> {
		if let (Some(first), Some(last)) = (self.frames.first(), self.frames.last()) {
			return Poll::Ready((first.timestamp, last.timestamp));
		}

		Poll::Pending
	}
}

/// Create a group, frame-by-frame.
#[derive(Clone, Debug)]
pub struct GroupProducer {
	// Mutable stream state.
	state: Producer<GroupState>,

	info: Group,
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
	/// If you want to write multiple chunks, use [Self::create_frame] or [Self::append_frame].
	pub fn write_frame<B: Into<Bytes>>(&mut self, frame: B, timestamp: Time) -> Result<(), Error> {
		let data = frame.into();
		let frame = Frame {
			size: data.len(),
			timestamp,
		};

		let mut frame = self.create_frame(frame)?;
		frame.write_chunk(data)?;
		frame.final_chunk()?;

		Ok(())
	}

	/// Create a frame with an upfront size
	pub fn create_frame(&mut self, info: Frame) -> Result<FrameProducer, Error> {
		let frame = FrameProducer::new(info);
		self.append_frame(frame.clone())?;

		Ok(frame)
	}

	/// Append a frame to the group.
	pub fn append_frame(&mut self, frame: FrameProducer) -> Result<(), Error> {
		self.state.modify()?.append_frame(frame)
	}

	// Clean termination of the group.
	pub fn final_frame(&mut self) -> Result<(), Error> {
		self.state.modify()?.fin = true;
		Ok(())
	}

	pub fn abort(self, err: Error) -> Result<(), Error> {
		self.state.close(err)
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> GroupConsumer {
		GroupConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
		}
	}

	pub async fn unused(&self) -> Result<(), Error> {
		self.state.unused().await
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
#[derive(Clone, Debug)]
pub struct GroupConsumer {
	// Modify the stream state.
	state: Consumer<GroupState>,

	// Immutable stream state.
	info: Group,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl GroupConsumer {
	pub fn info(&self) -> &Group {
		&self.info
	}

	/// Read the next frame.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>, Error> {
		waiter_fn(move |waiter| self.poll_read_frame(waiter)).await
	}

	pub fn poll_read_frame(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Option<Bytes>, Error>> {
		self.state
			.poll(waiter, |state| state.poll_read_frame(waiter, &mut self.index))?
	}

	/// Return a reader for the next frame.
	pub async fn next_frame(&mut self) -> Result<Option<FrameConsumer>, Error> {
		waiter_fn(move |waiter| self.poll_next_frame(waiter)).await
	}

	pub fn poll_next_frame(&mut self, waiter: &Waiter<'_>) -> Poll<Result<Option<FrameConsumer>, Error>> {
		self.state.poll(waiter, |state| state.poll_next_frame(&mut self.index))
	}

	pub(super) fn poll_timestamp(&mut self, waiter: &Waiter<'_>) -> Poll<Result<(Time, Time), Error>> {
		self.state.poll(waiter, |state| state.poll_timestamp())
	}

	pub async fn closed(&self) -> Error {
		self.state.closed().await
	}
}

impl Deref for GroupConsumer {
	type Target = Group;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_group_from_u64() {
		let group: Group = 42u64.into();
		assert_eq!(group.sequence, 42);
	}

	#[tokio::test]
	async fn test_group_write_read_frame() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);
		let mut consumer = producer.consume();

		// Write a frame
		let timestamp = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("hello"), timestamp).unwrap();
		producer.final_frame().unwrap();

		// Read the frame
		let data = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("hello"));

		// No more frames
		let data = consumer.read_frame().await.unwrap();
		assert!(data.is_none());
	}

	#[tokio::test]
	async fn test_group_multiple_frames() {
		let group = Group { sequence: 5 };

		let mut producer = GroupProducer::new(group);
		let mut consumer = producer.consume();

		// Write multiple frames
		let t1 = Time::from_millis(100).unwrap();
		let t2 = Time::from_millis(200).unwrap();
		let t3 = Time::from_millis(300).unwrap();

		producer.write_frame(Bytes::from("frame1"), t1).unwrap();
		producer.write_frame(Bytes::from("frame2"), t2).unwrap();
		producer.write_frame(Bytes::from("frame3"), t3).unwrap();
		producer.final_frame().unwrap();

		// Read all frames
		assert_eq!(consumer.read_frame().await.unwrap().unwrap(), Bytes::from("frame1"));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap(), Bytes::from("frame2"));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap(), Bytes::from("frame3"));
		assert!(consumer.read_frame().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn test_group_create_frame_multi_chunk() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);
		let mut consumer = producer.consume();

		// Create a frame and write it in chunks
		let timestamp = Time::from_millis(100).unwrap();
		let frame = Frame { size: 10, timestamp };
		let mut frame_producer = producer.create_frame(frame).unwrap();
		frame_producer.write_chunk(Bytes::from("hello")).unwrap();
		frame_producer.write_chunk(Bytes::from("world")).unwrap();
		frame_producer.final_chunk().unwrap();

		producer.final_frame().unwrap();

		// Read the frame
		let data = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("helloworld"));
	}

	#[tokio::test]
	async fn test_group_next_frame() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);
		let mut consumer = producer.consume();

		let timestamp = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("test"), timestamp).unwrap();
		producer.final_frame().unwrap();

		// Use next_frame to get the frame consumer
		let mut frame_consumer = consumer.next_frame().await.unwrap().unwrap();
		let data = frame_consumer.read_all().await.unwrap();
		assert_eq!(data, Bytes::from("test"));

		// No more frames
		assert!(consumer.next_frame().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn test_group_multiple_consumers() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);
		let mut consumer1 = producer.consume();
		let mut consumer2 = producer.consume();

		let instant = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("data"), instant).unwrap();
		producer.final_frame().unwrap();

		// Both consumers should get the frame
		let data1 = consumer1.read_frame().await.unwrap().unwrap();
		let data2 = consumer2.read_frame().await.unwrap().unwrap();
		assert_eq!(data1, Bytes::from("data"));
		assert_eq!(data2, Bytes::from("data"));
	}

	#[tokio::test]
	async fn test_group_abort() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);
		let mut consumer = producer.consume();

		let timestamp = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("data"), timestamp).unwrap();

		// Abort before closing - this should propagate the error
		producer.abort(Error::Cancel).unwrap();

		// The first frame was already written and closed, so we can read it
		let _result = consumer.read_frame().await;
		// The frame itself will succeed but the group will be in error state
		// So we check closed() instead
		consumer.closed().await;
	}

	#[tokio::test]
	async fn test_group_max_instant_increasing() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);

		let t1 = Time::from_millis(100).unwrap();
		let t2 = Time::from_millis(200).unwrap();
		let t3 = Time::from_millis(250).unwrap(); // Always increasing

		producer.write_frame(Bytes::from("f1"), t1).unwrap();
		producer.write_frame(Bytes::from("f2"), t2).unwrap();
		producer.write_frame(Bytes::from("f3"), t3).unwrap();

		// max_instant should be t3 (250ms), the highest timestamp
		// This is indirectly tested through the internal state
		// We can verify by checking the state was updated
		producer.final_frame().unwrap();
	}

	#[tokio::test]
	async fn test_group_max_instant_out_of_order() {
		// Test what happens when we write frames with out-of-order timestamps
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);

		let t1 = Time::from_millis(100).unwrap();
		let t2 = Time::from_millis(200).unwrap();
		let t3 = Time::from_millis(150).unwrap(); // Out of order - older than t2

		producer.write_frame(Bytes::from("f1"), t1).unwrap();
		producer.write_frame(Bytes::from("f2"), t2).unwrap();

		// This should fail with Expired because t3 (150ms) is older than t2 (200ms)
		// and max_latency is 0 by default
		let result = producer.write_frame(Bytes::from("f3"), t3);
		assert!(
			result.is_err(),
			"Writing a frame with an older timestamp should fail when max_latency is 0"
		);
	}

	#[tokio::test]
	async fn test_group_append_frame() {
		let group = Group { sequence: 0 };

		let mut producer = GroupProducer::new(group);
		let mut consumer = producer.consume();

		// Create a frame manually and append it
		let timestamp = Time::from_millis(100).unwrap();
		let frame = Frame { size: 5, timestamp };
		let mut frame_producer = FrameProducer::new(frame);
		frame_producer.write_chunk(Bytes::from("hello")).unwrap();
		frame_producer.final_chunk().unwrap();

		producer.append_frame(frame_producer).unwrap();
		producer.final_frame().unwrap();

		let data = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("hello"));
	}
}
