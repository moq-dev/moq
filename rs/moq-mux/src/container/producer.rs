use super::{Container, Frame};

/// A producer for media tracks that manages group boundaries.
///
/// Generic over `C: Container` to support different container encodings (Hang Legacy,
/// CMAF, …).
///
/// ## Group Management
///
/// Every group must start with a keyframe. Writing a frame with `keyframe = true`
/// closes the previous group (if any) and starts a new one. Writing a non-keyframe
/// frame when no group is open is a protocol violation.
///
/// [`finish_group`](Self::finish_group) closes the current group early; the next write
/// must be a keyframe. This is useful for streams without inherent keyframes
/// (e.g. audio) that mark every Nth frame as a keyframe but want to flush the
/// current group immediately rather than waiting for the next keyframe to arrive.
///
/// ## Latency Buffering
///
/// When `latency` is zero (default), each frame is written immediately as its own
/// container frame. When non-zero, frames are buffered and flushed together when:
/// - A keyframe arrives (flushes the previous group's buffer, starts new group),
/// - The buffered duration exceeds `latency`,
/// - `finish()` is called.
///
/// This is useful for CMAF where multiple samples should be packed into one moof+mdat.
pub struct Producer<C: Container> {
	pub track: moq_lite::TrackProducer,
	container: C,
	group: Option<moq_lite::GroupProducer>,
	buffer: Vec<Frame>,

	latency: std::time::Duration,
}

impl<C: Container> Producer<C> {
	/// Create a new Producer wrapping the given moq-lite producer.
	pub fn new(track: moq_lite::TrackProducer, container: C) -> Self {
		Self {
			track,
			container,
			group: None,
			buffer: Vec::new(),
			latency: std::time::Duration::ZERO,
		}
	}

	/// Set the maximum buffering latency.
	///
	/// When non-zero, frames are buffered and flushed together when the buffered
	/// duration exceeds this value, or when a keyframe arrives. This allows packing
	/// multiple samples into a single container frame (e.g. CMAF moof+mdat).
	///
	/// Default is zero (flush immediately).
	pub fn with_latency(mut self, latency: std::time::Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Write a frame to the track.
	///
	/// A keyframe closes any open group and starts a new one. A non-keyframe extends
	/// the current group; if no group is open, returns a protocol violation.
	pub fn write(&mut self, frame: Frame) -> Result<(), C::Error> {
		// Close the current group on an explicit keyframe.
		if frame.keyframe {
			self.finish_group()?;
		}

		// Start a new group if needed; the first frame of a group must be a keyframe.
		if self.group.is_none() {
			if !frame.keyframe {
				return Err(moq_lite::Error::ProtocolViolation.into());
			}
			self.group = Some(self.track.append_group()?);
		}

		// Buffer or write the frame.
		if self.latency.is_zero() {
			let group = self.group.as_mut().unwrap();
			self.container.write(group, &[frame])?;
		} else {
			self.buffer.push(frame);

			// Check if buffered duration exceeds latency.
			if self.buffer.len() >= 2 {
				let first_ts: std::time::Duration = self.buffer.first().unwrap().timestamp.into();
				let last_ts: std::time::Duration = self.buffer.last().unwrap().timestamp.into();

				if last_ts.saturating_sub(first_ts) >= self.latency {
					self.flush()?;
				}
			}
		}

		Ok(())
	}

	/// Close the current group early, flushing any buffered frames.
	///
	/// The next [`write`](Self::write) must be a keyframe.
	pub fn finish_group(&mut self) -> Result<(), C::Error> {
		self.flush()?;
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		Ok(())
	}

	/// Flush any buffered frames into the current group without closing it.
	fn flush(&mut self) -> Result<(), C::Error> {
		if self.buffer.is_empty() {
			return Ok(());
		}

		let group = match &mut self.group {
			Some(group) => group,
			None => return Ok(()),
		};

		self.container.write(group, &self.buffer)?;
		self.buffer.clear();

		Ok(())
	}

	/// Finish the track, flushing any buffered frames and closing any open group.
	pub fn finish(&mut self) -> Result<(), C::Error> {
		self.finish_group()?;
		self.track.finish()?;
		Ok(())
	}

	/// Create a consumer for this track.
	pub fn consume(&self) -> moq_lite::TrackConsumer {
		self.track.consume()
	}
}

impl<C: Container> std::ops::Deref for Producer<C> {
	type Target = moq_lite::TrackProducer;

	fn deref(&self) -> &Self::Target {
		&self.track
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use super::*;
	use crate::container::{Hang, Timestamp};

	fn frame(timestamp_us: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
		}
	}

	/// Drain all groups from a finished track, returning their frame counts.
	async fn collect_groups(mut consumer: moq_lite::TrackConsumer) -> Vec<usize> {
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
			let mut count = 0;
			while group.next_frame().await.unwrap().is_some() {
				count += 1;
			}
			groups.push(count);
		}
		groups
	}

	/// Explicit keyframe closes the current group and starts a new one.
	#[tokio::test]
	async fn keyframe_closes_group_immediately() {
		let track = moq_lite::Track::new("test").produce();
		let consumer = track.consume();
		let mut producer = Producer::new(track, Hang::Legacy);

		producer.write(frame(0, true)).unwrap(); // first frame must be a keyframe
		producer.write(frame(10_000, false)).unwrap();
		producer.write(frame(20_000, true)).unwrap(); // keyframe → new group
		producer.write(frame(30_000, false)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 2]);
	}

	/// `finish_group()` flushes the current group immediately; the next write must be a keyframe.
	#[tokio::test]
	async fn finish_group_closes_immediately() {
		let track = moq_lite::Track::new("test").produce();
		let consumer = track.consume();
		let mut producer = Producer::new(track, Hang::Legacy);

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer.finish_group().unwrap();
		producer.write(frame(20_000, true)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 1]);
	}

	/// Writing a non-keyframe with no open group is a protocol violation.
	#[test]
	fn first_frame_must_be_keyframe() {
		let track = moq_lite::Track::new("test").produce();
		let mut producer = Producer::new(track, Hang::Legacy);

		let err = producer.write(frame(0, false)).unwrap_err();
		assert!(matches!(err, crate::Error::Moq(moq_lite::Error::ProtocolViolation)));
	}
}
