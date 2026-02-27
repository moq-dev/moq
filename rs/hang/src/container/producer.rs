use super::{Frame, OrderedConsumer, Timestamp};
use crate::Error;

/// A producer for media tracks with group management.
///
/// This wraps a `moq_lite::TrackProducer` and adds hang-specific functionality
/// like automatic timestamp encoding and group management.
///
/// ## Group Management
///
/// Groups can be managed explicitly via [`keyframe()`](Self::keyframe) or automatically
/// via [`with_max_group_duration()`](Self::with_max_group_duration):
/// - Explicit: call `keyframe()` before writing a keyframe to start a new group.
/// - Automatic: set a max group duration and groups are created/closed based on timestamps.
#[derive(Clone)]
pub struct OrderedProducer {
	pub track: moq_lite::TrackProducer,
	group: Option<moq_lite::GroupProducer>,

	// The timestamp of the first frame in the current group.
	group_start: Option<Timestamp>,

	// The previous frame's timestamp, used to estimate frame interval.
	prev_timestamp: Option<Timestamp>,

	// When set, automatically manage group boundaries based on duration.
	max_group_duration: Option<Timestamp>,

	// Whether keyframe() was called and the next write() should start a new group.
	pending_keyframe: bool,
}

impl OrderedProducer {
	/// Create a new OrderedProducer wrapping the given moq-lite producer.
	pub fn new(inner: moq_lite::TrackProducer) -> Self {
		Self {
			track: inner,
			group: None,
			group_start: None,
			prev_timestamp: None,
			max_group_duration: None,
			pending_keyframe: false,
		}
	}

	/// Create a new OrderedProducer with automatic group duration management.
	///
	/// Groups will be automatically closed and new ones started when the estimated
	/// next frame would exceed the max group duration.
	pub fn with_max_group_duration(inner: moq_lite::TrackProducer, duration: Timestamp) -> Self {
		Self {
			track: inner,
			group: None,
			group_start: None,
			prev_timestamp: None,
			max_group_duration: Some(duration),
			pending_keyframe: false,
		}
	}

	/// Signal that the next frame starts a new group (keyframe).
	///
	/// Finishes the current group if one exists. The next call to `write()`
	/// will create a new group.
	pub fn keyframe(&mut self) -> Result<(), Error> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.pending_keyframe = true;
		Ok(())
	}

	/// Write a frame to the track.
	///
	/// The frame's timestamp is automatically encoded as a header.
	///
	/// All frames should be in *decode order*.
	///
	/// Group boundaries are managed either:
	/// - Explicitly: call `keyframe()` before writing a keyframe.
	/// - Automatically: if `max_group_duration` is set, groups close when the
	///   estimated next frame would exceed the duration.
	pub fn write(&mut self, frame: Frame) -> Result<(), Error> {
		tracing::trace!(?frame, "write frame");

		// Check if we should auto-close the current group based on duration.
		if let (Some(max_duration), Some(group_start), Some(prev_timestamp)) =
			(self.max_group_duration, self.group_start, self.prev_timestamp)
			&& self.group.is_some()
		{
			// Estimate the next frame's timestamp assuming constant FPS.
			let frame_interval = frame.timestamp.checked_sub(prev_timestamp).unwrap_or(Timestamp::ZERO);
			let estimated_next = frame.timestamp.checked_add(frame_interval)?;

			if estimated_next.checked_sub(group_start).unwrap_or(Timestamp::ZERO) >= max_duration
				&& let Some(mut group) = self.group.take()
			{
				group.finish()?;
			}
		}

		// Start a new group if needed (first frame, after keyframe(), or after auto-close).
		if self.group.is_none() || self.pending_keyframe {
			if let Some(mut group) = self.group.take() {
				group.finish()?;
			}

			let group = self.track.append_group()?;
			self.group = Some(group);
			self.group_start = Some(frame.timestamp);
			self.pending_keyframe = false;
		}

		let mut group = self.group.take().expect("group should exist");
		frame.encode(&mut group)?;
		self.group.replace(group);

		self.prev_timestamp = Some(frame.timestamp);

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
