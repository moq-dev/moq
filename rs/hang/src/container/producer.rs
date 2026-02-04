use super::{Frame, OrderedConsumer, Timestamp};
use crate::Error;

/// A producer for media tracks with keyframe-based group management.
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
pub struct OrderedProducer {
	pub track: moq_lite::TrackProducer,
	group: Option<moq_lite::GroupProducer>,
	keyframe: Option<Timestamp>,
}

impl OrderedProducer {
	/// Create a new OrderedProducer wrapping the given moq-lite producer.
	pub fn new(inner: moq_lite::TrackProducer) -> Self {
		Self {
			track: inner,
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
		tracing::trace!(?frame, "write frame");

		if frame.keyframe {
			if let Some(group) = self.group.take() {
				group.close();
			}

			// Make sure this frame's timestamp doesn't go backwards relative to the last keyframe.
			// We can't really enforce this for frames generally because b-frames suck.
			if let Some(keyframe) = self.keyframe
				&& frame.timestamp < keyframe
			{
				return Err(Error::TimestampBackwards);
			}

			self.keyframe = Some(frame.timestamp);
		}

		let mut group = match self.group.take() {
			Some(group) => group,
			None if frame.keyframe => self.track.append_group(),
			// The first frame must be a keyframe.
			None => return Err(Error::MissingKeyframe),
		};

		frame.encode(&mut group)?;

		self.group.replace(group);

		Ok(())
	}

	/// An explicit way to end the current group.
	///
	/// This is useful to flush when you know the next frame will be a keyframe.
	pub fn flush(&mut self) -> Result<(), Error> {
		self.group.take().ok_or(Error::MissingKeyframe)?.close();
		Ok(())
	}

	/// Create a consumer for this track.
	///
	/// Multiple consumers can be created from the same producer, each receiving
	/// a copy of all data written to the track.
	pub fn consume(&self, max_latency: std::time::Duration) -> OrderedConsumer {
		OrderedConsumer::new(self.track.consume(), max_latency)
	}
}

impl From<moq_lite::TrackProducer> for OrderedProducer {
	fn from(inner: moq_lite::TrackProducer) -> Self {
		Self::new(inner)
	}
}

impl std::ops::Deref for OrderedProducer {
	type Target = moq_lite::TrackProducer;

	fn deref(&self) -> &Self::Target {
		&self.track
	}
}
