//! A group is a stream of frames, split into a [GroupProducer] and [GroupConsumer] handle.
//!
//! A [GroupProducer] writes an ordered stream of frames.
//! Frames can be written all at once, or in chunks.
//!
//! A [GroupConsumer] reads an ordered stream of frames.
//! The reader can be cloned, in which case each reader receives a copy of each frame. (fanout)
//!
//! The stream is closed with [Error] when all writers or readers are dropped.
use std::task::{Poll, ready};

use bytes::Bytes;

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
	// The frames that have been written thus far.
	// We store producers so consumers can be created on-demand.
	frames: Vec<FrameProducer>,

	// Whether the group has been finalized (no more frames).
	fin: bool,

	// The error that caused the group to be aborted, if any.
	abort: Option<Error>,
}

impl GroupState {
	fn poll_get_frame(&self, index: usize) -> Poll<Result<Option<FrameConsumer>>> {
		if let Some(frame) = self.frames.get(index) {
			Poll::Ready(Ok(Some(frame.consume())))
		} else if self.fin {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn poll_finished(&self) -> Poll<Result<u64>> {
		if self.fin {
			Poll::Ready(Ok(self.frames.len() as u64))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}
}

fn modify(state: &conducer::Producer<GroupState>) -> Result<conducer::Mut<'_, GroupState>> {
	state.write().map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
}

/// Writes frames to a group in order.
///
/// Each group is delivered independently over a QUIC stream.
/// Use [Self::write_frame] for simple single-buffer frames,
/// or [Self::create_frame] for multi-chunk streaming writes.
pub struct GroupProducer {
	// Mutable stream state.
	state: conducer::Producer<GroupState>,

	/// The group header containing the sequence number.
	pub info: Group,
}

impl GroupProducer {
	/// Create a new group producer.
	pub fn new(info: Group) -> Self {
		Self {
			info,
			state: conducer::Producer::default(),
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
		frame.write(data)?;
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
		let mut state = modify(&self.state)?;
		if state.fin {
			return Err(Error::Closed);
		}
		state.frames.push(frame);
		Ok(())
	}

	/// Return the number of frames written so far.
	pub fn frame_count(&self) -> usize {
		self.state.read().frames.len()
	}

	/// Mark the group as complete; no more frames will be written.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = modify(&self.state)?;
		state.fin = true;
		Ok(())
	}

	/// Abort the group with the given error.
	///
	/// No updates can be made after this point.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut guard = modify(&self.state)?;

		// Abort all frames still in progress.
		for frame in guard.frames.iter_mut() {
			// Ignore errors, we don't care if the frame was already closed.
			frame.abort(err.clone()).ok();
		}

		guard.abort = Some(err);
		guard.close();
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

	/// Block until the group is closed or aborted.
	pub async fn closed(&self) -> Error {
		self.state.closed().await;
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
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
	state: conducer::Consumer<GroupState>,

	// Immutable stream state.
	pub info: Group,

	// The number of frames we've read.
	// NOTE: Cloned readers inherit this offset, but then run in parallel.
	index: usize,
}

impl GroupConsumer {
	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&conducer::Ref<'_, GroupState>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Block until the frame at the given index is available.
	///
	/// Returns None if the group is finished and the index is out of range.
	pub async fn get_frame(&self, index: usize) -> Result<Option<FrameConsumer>> {
		conducer::wait(|waiter| self.poll_get_frame(waiter, index)).await
	}

	/// Poll for the frame at the given index, without blocking.
	///
	/// Returns None if the group is finished and the index is out of range.
	pub fn poll_get_frame(&self, waiter: &conducer::Waiter, index: usize) -> Poll<Result<Option<FrameConsumer>>> {
		self.poll(waiter, |state| state.poll_get_frame(index))
	}

	/// Return a consumer for the next frame for chunked reading.
	pub async fn next_frame(&mut self) -> Result<Option<FrameConsumer>> {
		conducer::wait(|waiter| self.poll_next_frame(waiter)).await
	}

	/// Poll for the next frame, without blocking.
	///
	/// Returns None if the group is finished and the index is out of range.
	pub fn poll_next_frame(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<FrameConsumer>>> {
		let Some(frame) = ready!(self.poll(waiter, |state| state.poll_get_frame(self.index))?) else {
			return Poll::Ready(Ok(None));
		};

		self.index += 1;
		Poll::Ready(Ok(Some(frame)))
	}

	/// Read the next frame's data all at once, without blocking.
	pub fn poll_read_frame(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Bytes>>> {
		let Some(mut frame) = ready!(self.poll(waiter, |state| state.poll_get_frame(self.index))?) else {
			return Poll::Ready(Ok(None));
		};

		let data = ready!(frame.poll_read_all(waiter))?;
		self.index += 1;

		Poll::Ready(Ok(Some(data)))
	}

	/// Read the next frame's data all at once.
	pub async fn read_frame(&mut self) -> Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Read all of the chunks of the next frame, without blocking.
	pub fn poll_read_frame_chunks(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Vec<Bytes>>>> {
		let Some(mut frame) = ready!(self.poll(waiter, |state| state.poll_get_frame(self.index))?) else {
			return Poll::Ready(Ok(None));
		};

		let data = ready!(frame.poll_read_all_chunks(waiter))?;
		self.index += 1;

		Poll::Ready(Ok(Some(data)))
	}

	/// Read all of the chunks of the next frame.
	pub async fn read_frame_chunks(&mut self) -> Result<Option<Vec<Bytes>>> {
		conducer::wait(|waiter| self.poll_read_frame_chunks(waiter)).await
	}

	/// Poll for the final number of frames in the group.
	pub fn poll_finished(&mut self, waiter: &conducer::Waiter) -> Poll<Result<u64>> {
		self.poll(waiter, |state| state.poll_finished())
	}

	/// Block until the group is finished, returning the number of frames in the group.
	pub async fn finished(&mut self) -> Result<u64> {
		conducer::wait(|waiter| self.poll_finished(waiter)).await
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	#[test]
	fn basic_frame_reading() {
		let mut producer = Group { sequence: 0 }.produce();
		producer.write_frame(Bytes::from_static(b"frame0")).unwrap();
		producer.write_frame(Bytes::from_static(b"frame1")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let f0 = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.info.size, 6);
		let f1 = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f1.info.size, 6);
		let end = consumer.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	#[test]
	fn read_frame_all_at_once() {
		let mut producer = Group { sequence: 0 }.produce();
		producer.write_frame(Bytes::from_static(b"hello")).unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let data = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(data, Bytes::from_static(b"hello"));
	}

	#[test]
	fn read_frame_chunks() {
		let mut producer = Group { sequence: 0 }.produce();
		let mut frame = producer.create_frame(Frame { size: 10 }).unwrap();
		frame.write(Bytes::from_static(b"hello")).unwrap();
		frame.write(Bytes::from_static(b"world")).unwrap();
		frame.finish().unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let chunks = consumer.read_frame_chunks().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(chunks.len(), 2);
		assert_eq!(chunks[0], Bytes::from_static(b"hello"));
		assert_eq!(chunks[1], Bytes::from_static(b"world"));
	}

	#[test]
	fn get_frame_by_index() {
		let mut producer = Group { sequence: 0 }.produce();
		producer.write_frame(Bytes::from_static(b"a")).unwrap();
		producer.write_frame(Bytes::from_static(b"bb")).unwrap();
		producer.finish().unwrap();

		let consumer = producer.consume();
		let f0 = consumer.get_frame(0).now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f0.info.size, 1);
		let f1 = consumer.get_frame(1).now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f1.info.size, 2);
		let f2 = consumer.get_frame(2).now_or_never().unwrap().unwrap();
		assert!(f2.is_none());
	}

	#[test]
	fn group_finish_returns_none() {
		let mut producer = Group { sequence: 0 }.produce();
		producer.finish().unwrap();

		let mut consumer = producer.consume();
		let end = consumer.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}

	#[test]
	fn abort_propagates() {
		let mut producer = Group { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		producer.abort(crate::Error::Cancel).unwrap();

		let result = consumer.next_frame().now_or_never().unwrap();
		assert!(matches!(result, Err(crate::Error::Cancel)));
	}

	#[tokio::test]
	async fn pending_then_ready() {
		let mut producer = Group { sequence: 0 }.produce();
		let mut consumer = producer.consume();

		// Consumer blocks because no frames yet.
		assert!(consumer.next_frame().now_or_never().is_none());

		producer.write_frame(Bytes::from_static(b"data")).unwrap();
		producer.finish().unwrap();

		let frame = consumer.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.info.size, 4);
	}

	#[test]
	fn clone_consumer_independent() {
		let mut producer = Group { sequence: 0 }.produce();
		producer.write_frame(Bytes::from_static(b"a")).unwrap();

		let mut c1 = producer.consume();
		// Read one frame from c1
		let _ = c1.next_frame().now_or_never().unwrap().unwrap().unwrap();

		// Clone c1 — inherits index (past first frame)
		let mut c2 = c1.clone();

		producer.write_frame(Bytes::from_static(b"b")).unwrap();
		producer.finish().unwrap();

		// c2 should get the second frame (inherited index)
		let f = c2.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(f.info.size, 1); // "b"

		let end = c2.next_frame().now_or_never().unwrap().unwrap();
		assert!(end.is_none());
	}
}
