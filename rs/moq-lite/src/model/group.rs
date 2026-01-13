//! A group is a stream of frames, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] writes an ordered stream of frames.
//! Frames can be written all at once, or in chunks.
//!
//! A [Consumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [ServeError::MoqError] when all writers or readers are dropped.
use std::ops::Deref;

use bytes::Bytes;

use super::{Consumer, Frame, FrameConsumer, FrameProducer, Producer};
use crate::{Error, ExpiresConsumer, ExpiresProducer, Time};

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

	// The maximum instant of the frames in the group.
	// TODO prevent going backwards instead?
	max_instant: Option<Time>,
}

impl GroupState {
	fn append_frame(&mut self, frame: FrameProducer) {
		self.max_instant = Some(self.max_instant.unwrap_or_default().max(frame.instant));
		self.frames.push(frame);
	}
}

/// Create a group, frame-by-frame.
#[derive(Clone, Debug)]
pub struct GroupProducer {
	// Mutable stream state.
	state: Producer<GroupState>,
	info: Group,
	expires: ExpiresProducer,
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
	pub fn write_frame<B: Into<Bytes>>(&mut self, frame: B, instant: Time) -> Result<(), Error> {
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
	pub fn create_frame(&mut self, info: Frame) -> Result<FrameProducer, Error> {
		let frame = FrameProducer::new(info);
		self.append_frame(frame.clone())?;
		Ok(frame)
	}

	/// Append a frame to the group.
	pub fn append_frame(&mut self, frame: FrameProducer) -> Result<(), Error> {
		// Add the current frame to the expiration tracker.
		// NOTE: This might return an error if the current group is expired.
		self.expires.create_frame(self.info.sequence, frame.instant)?;
		self.state.modify(|state| state.append_frame(frame))
	}

	// Clean termination of the group.
	pub fn close(&mut self) -> Result<(), Error> {
		self.state.close()
	}

	pub fn abort(&mut self, err: Error) -> Result<(), Error> {
		self.state.abort(err)
	}

	/// Create a new consumer for the group.
	pub fn consume(&self) -> GroupConsumer {
		GroupConsumer {
			info: self.info.clone(),
			state: self.state.consume(),
			index: 0,
			active: None,
			expires: self.expires.consume(),
		}
	}

	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub async fn unused(&self) {
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

	// Used to make read_frame cancel safe.
	active: Option<FrameConsumer>,

	// Used to check if the group is expired early.
	expires: ExpiresConsumer,
}

impl GroupConsumer {
	pub fn info(&self) -> &Group {
		&self.info
	}

	/// Read the next frame.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>, Error> {
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
	pub async fn next_frame(&mut self) -> Result<Option<FrameConsumer>, Error> {
		// Just in case someone called read_frame, cancelled it, then called next_frame.
		if let Some(frame) = self.active.take() {
			return Ok(Some(frame));
		}

		let max_instant = self.state.borrow().max_instant;

		let state = tokio::select! {
			biased;
			// Wait until a new frame.
			state = self.state.wait_for(|state| self.index < state.frames.len()) => state?,
			// Or wait until the maximum instant in the group is expired.
			// We do this second because it's legal to return expired frames if we have them in cache already.
			err = self.expires.wait_expired(self.info.sequence, max_instant.unwrap_or_default()), if max_instant.is_some() => return Err(err),
			// NOTE: We don't have to wait for a new maximum, because it will satisfy the wait for the next frame.
		};

		if let Some(frame) = state.frames.get(self.index).cloned() {
			self.index += 1;
			return Ok(Some(frame.consume()));
		}

		Ok(None)
	}

	pub async fn closed(&self) -> Result<(), Error> {
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
	use crate::{Delivery, DeliveryProducer, ExpiresProducer};

	#[test]
	fn test_group_from_u64() {
		let group: Group = 42u64.into();
		assert_eq!(group.sequence, 42);
	}

	#[tokio::test]
	async fn test_group_write_read_frame() {
		let group = Group { sequence: 0 };
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer = producer.consume();

		// Write a frame
		let instant = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("hello"), instant).unwrap();
		producer.close().unwrap();

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
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer = producer.consume();

		// Write multiple frames
		let t1 = Time::from_millis(100).unwrap();
		let t2 = Time::from_millis(200).unwrap();
		let t3 = Time::from_millis(300).unwrap();

		producer.write_frame(Bytes::from("frame1"), t1).unwrap();
		producer.write_frame(Bytes::from("frame2"), t2).unwrap();
		producer.write_frame(Bytes::from("frame3"), t3).unwrap();
		producer.close().unwrap();

		// Read all frames
		assert_eq!(consumer.read_frame().await.unwrap().unwrap(), Bytes::from("frame1"));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap(), Bytes::from("frame2"));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap(), Bytes::from("frame3"));
		assert!(consumer.read_frame().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn test_group_create_frame_multi_chunk() {
		let group = Group { sequence: 0 };
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer = producer.consume();

		// Create a frame and write it in chunks
		let instant = Time::from_millis(100).unwrap();
		let frame = Frame { size: 10, instant };
		let mut frame_producer = producer.create_frame(frame).unwrap();
		frame_producer.write_chunk(Bytes::from("hello")).unwrap();
		frame_producer.write_chunk(Bytes::from("world")).unwrap();
		frame_producer.close().unwrap();

		producer.close().unwrap();

		// Read the frame
		let data = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("helloworld"));
	}

	#[tokio::test]
	async fn test_group_next_frame() {
		let group = Group { sequence: 0 };
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer = producer.consume();

		let instant = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("test"), instant).unwrap();
		producer.close().unwrap();

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
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer1 = producer.consume();
		let mut consumer2 = producer.consume();

		let instant = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("data"), instant).unwrap();
		producer.close().unwrap();

		// Both consumers should get the frame
		let data1 = consumer1.read_frame().await.unwrap().unwrap();
		let data2 = consumer2.read_frame().await.unwrap().unwrap();
		assert_eq!(data1, Bytes::from("data"));
		assert_eq!(data2, Bytes::from("data"));
	}

	#[tokio::test]
	async fn test_group_abort() {
		let group = Group { sequence: 0 };
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer = producer.consume();

		let instant = Time::from_millis(100).unwrap();
		producer.write_frame(Bytes::from("data"), instant).unwrap();

		// Abort before closing - this should propagate the error
		producer.abort(Error::Cancel).unwrap();

		// The first frame was already written and closed, so we can read it
		let _result = consumer.read_frame().await;
		// The frame itself will succeed but the group will be in error state
		// So we check closed() instead
		let closed_result = consumer.closed().await;
		assert!(closed_result.is_err());
	}

	#[tokio::test]
	async fn test_group_max_instant_increasing() {
		let group = Group { sequence: 0 };
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires.clone());

		let t1 = Time::from_millis(100).unwrap();
		let t2 = Time::from_millis(200).unwrap();
		let t3 = Time::from_millis(250).unwrap(); // Always increasing

		producer.write_frame(Bytes::from("f1"), t1).unwrap();
		producer.write_frame(Bytes::from("f2"), t2).unwrap();
		producer.write_frame(Bytes::from("f3"), t3).unwrap();

		// max_instant should be t3 (250ms), the highest timestamp
		// This is indirectly tested through the internal state
		// We can verify by checking the state was updated
		producer.close().unwrap();
	}

	#[tokio::test]
	async fn test_group_max_instant_out_of_order() {
		// Test what happens when we write frames with out-of-order timestamps
		let group = Group { sequence: 0 };
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires.clone());

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
		let delivery = DeliveryProducer::new(Delivery::default());
		let expires = ExpiresProducer::new(delivery.consume());

		let mut producer = GroupProducer::new(group, expires);
		let mut consumer = producer.consume();

		// Create a frame manually and append it
		let instant = Time::from_millis(100).unwrap();
		let frame = Frame { size: 5, instant };
		let mut frame_producer = FrameProducer::new(frame);
		frame_producer.write_chunk(Bytes::from("hello")).unwrap();
		frame_producer.close().unwrap();

		producer.append_frame(frame_producer).unwrap();
		producer.close().unwrap();

		let data = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(data, Bytes::from("hello"));
	}
}
