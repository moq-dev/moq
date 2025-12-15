use std::collections::VecDeque;
use std::ops::Deref;

use crate::model::{Frame, GroupConsumer, Timestamp};
use crate::Error;
use futures::{stream::FuturesUnordered, StreamExt};

use moq_lite::{coding::*, lite};

/// A producer for media tracks.
///
/// This wraps a `moq_lite::TrackProducer` and adds hang-specific functionality
/// like automatic timestamp encoding and keyframe-based group management.
///
/// ## Group Management
///
/// Groups are automatically created and managed based on keyframes:
/// - When a keyframe is written, the current group is finished and a new one begins.
/// - Non-keyframes are appended to the current group.
/// - Each frame includes a timestamp header for proper playback timing.
#[derive(Clone)]
pub struct TrackProducer {
	pub inner: moq_lite::TrackProducer,
	group: Option<moq_lite::GroupProducer>,
	keyframe: Option<Timestamp>,
}

impl TrackProducer {
	/// Create a new TrackProducer wrapping the given moq-lite producer.
	pub fn new(inner: moq_lite::TrackProducer) -> Self {
		Self {
			inner,
			group: None,
			keyframe: None,
		}
	}

	/// Write a frame to the track.
	///
	/// The frame's timestamp is automatically encoded as a header, and keyframes
	/// trigger the creation of new groups for efficient seeking and caching.
	///
	/// All frames should be in *decode order*.
	///
	/// The timestamp is usually monotonically increasing, but it depends on the encoding.
	/// For example, H.264 B-frames will introduce jitter and reordering.
	pub fn write(&mut self, frame: Frame) -> Result<(), Error> {
		let mut header = BytesMut::new();
		frame.timestamp.as_micros().encode(&mut header, lite::Version::Draft02);

		if frame.keyframe {
			if let Some(group) = self.group.take() {
				group.close();
			}

			// Make sure this frame's timestamp doesn't go backwards relative to the last keyframe.
			// We can't really enforce this for frames generally because b-frames suck.
			if let Some(keyframe) = self.keyframe {
				if frame.timestamp < keyframe {
					return Err(Error::TimestampBackwards);
				}
			}

			self.keyframe = Some(frame.timestamp);
		}

		let mut group = match self.group.take() {
			Some(group) => group,
			None if frame.keyframe => self.inner.append_group(),
			// The first frame must be a keyframe.
			None => return Err(Error::MissingKeyframe),
		};

		let size = header.len() + frame.payload.len();
		let mut chunked = group.create_frame(size.into());
		chunked.write_chunk(header.freeze());
		chunked.write_chunk(frame.payload);
		chunked.close();

		self.group.replace(group);

		Ok(())
	}

	/// A more efficient way to write a frame with multiple chunks.
	pub fn write_chunks(
		&mut self,
		keyframe: bool,
		timestamp: Timestamp,
		chunks: impl Iterator<Item = Bytes> + Clone,
	) -> Result<(), Error> {
		let mut header = BytesMut::new();
		timestamp.as_micros().encode(&mut header, lite::Version::Draft02);

		if keyframe {
			if let Some(group) = self.group.take() {
				group.close();
			}

			// Make sure this frame's timestamp doesn't go backwards relative to the last keyframe.
			// We can't really enforce this for frames generally because b-frames suck.
			if let Some(keyframe) = self.keyframe {
				if timestamp < keyframe {
					return Err(Error::TimestampBackwards);
				}
			}

			self.keyframe = Some(timestamp);
		}

		let mut group = match self.group.take() {
			Some(group) => group,
			None if keyframe => self.inner.append_group(),
			// The first frame must be a keyframe.
			None => return Err(Error::MissingKeyframe),
		};

		let size = chunks.clone().map(|c| c.len()).sum::<usize>();
		let mut chunked = group.create_frame((header.len() + size).into());
		chunked.write_chunk(header.freeze());
		for chunk in chunks {
			chunked.write_chunk(chunk);
		}
		chunked.close();

		self.group.replace(group);

		Ok(())
	}

	/// Create a consumer for this track.
	///
	/// Multiple consumers can be created from the same producer, each receiving
	/// a copy of all data written to the track.
	pub fn consume(&self) -> TrackConsumer {
		TrackConsumer::new(self.inner.consume())
	}
}

impl From<moq_lite::TrackProducer> for TrackProducer {
	fn from(inner: moq_lite::TrackProducer) -> Self {
		Self::new(inner)
	}
}

impl Deref for TrackProducer {
	type Target = moq_lite::TrackProducer;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

/// A consumer for hang-formatted media tracks.
///
/// This wraps a `moq_lite::TrackConsumer` and adds hang-specific functionality
/// like timestamp decoding, latency management, and frame buffering.
///
/// ## Latency Management
///
/// The consumer can skip groups that are too far behind to maintain low latency.
/// Use [`set_latency`](Self::set_latency) to configure the maximum acceptable delay.
pub struct TrackConsumer {
	pub inner: moq_lite::TrackConsumer,

	// The current group that we are reading from.
	current: Option<GroupConsumer>,

	// Future groups that we are monitoring, deciding based on [latency] whether to skip.
	pending: VecDeque<GroupConsumer>,

	// The maximum timestamp seen thus far, or zero because that's easier than None.
	max_timestamp: Timestamp,

	// The maximum buffer size before skipping a group.
	latency: std::time::Duration,
}

impl TrackConsumer {
	/// Create a new TrackConsumer wrapping the given moq-lite consumer.
	pub fn new(inner: moq_lite::TrackConsumer) -> Self {
		Self {
			inner,
			current: None,
			pending: VecDeque::new(),
			max_timestamp: Timestamp::default(),
			latency: std::time::Duration::ZERO,
		}
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and latency management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured latency target.
	///
	/// Returns `None` when the track has ended.
	pub async fn read_frame(&mut self) -> Result<Option<Frame>, Error> {
		let latency = self.latency.try_into()?;
		loop {
			let cutoff = self
				.max_timestamp
				.checked_add(latency)
				.ok_or(crate::TimestampOverflow)?;

			// Keep track of all pending groups, buffering until we detect a timestamp far enough in the future.
			// This is a race; only the first group will succeed.
			// TODO is there a way to do this without FuturesUnordered?
			let mut buffering = FuturesUnordered::new();
			for (index, pending) in self.pending.iter_mut().enumerate() {
				buffering.push(async move { (index, pending.buffer_until(cutoff).await) })
			}

			tokio::select! {
				biased;
				Some(res) = async { Some(self.current.as_mut()?.read().await) } => {
					drop(buffering);

					match res {
						// Got the next frame.
						Ok(Some(frame)) => {
							self.max_timestamp = frame.timestamp;
							return Ok(Some(frame));
						}
						Ok(None) | Err(_) => {
							// Group ended, instantly move to the next group.
							// We don't care about errors, which will happen if the group is closed early.
							self.current = self.pending.pop_front();
							continue;
						}
					};
				},
				Some(res) = async { self.inner.next_group().await.transpose() } => {
					let group = GroupConsumer::new(res?);
					drop(buffering);

					match self.current.as_ref() {
						Some(current) if group.info.sequence < current.info.sequence => {
							// Ignore old groups
							tracing::debug!(old = ?group.info.sequence, current = ?current.info.sequence, "skipping old group");
						},
						Some(_) => {
							// Insert into pending based on the sequence number ascending.
							let index = self.pending.partition_point(|g| g.info.sequence < group.info.sequence);
							self.pending.insert(index, group);
						},
						None => self.current = Some(group),
					};
				},
				Some((index, timestamp)) = buffering.next() => {
					if self.current.is_some() {
						tracing::debug!(old = ?self.max_timestamp, new = ?timestamp, buffer = ?self.latency, "skipping slow group");
					}

					drop(buffering);

					if index > 0 {
						self.pending.drain(0..index);
						tracing::debug!(count = index, "skipping additional groups");
					}

					self.current = self.pending.pop_front();
				}
				else => return Ok(None),
			}
		}
	}

	/// Set the maximum latency tolerance for this consumer.
	///
	/// Groups with timestamps older than `max_timestamp - latency` will be skipped.
	pub fn set_latency(&mut self, max: std::time::Duration) {
		self.latency = max;
	}

	/// Wait until the track is closed.
	pub async fn closed(&self) -> Result<(), Error> {
		Ok(self.inner.closed().await?)
	}
}

impl From<moq_lite::TrackConsumer> for TrackConsumer {
	fn from(inner: moq_lite::TrackConsumer) -> Self {
		Self::new(inner)
	}
}

impl From<TrackConsumer> for moq_lite::TrackConsumer {
	fn from(inner: TrackConsumer) -> Self {
		inner.inner
	}
}

impl Deref for TrackConsumer {
	type Target = moq_lite::TrackConsumer;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}
