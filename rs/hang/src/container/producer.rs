use super::{Frame, Timestamp};
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

	// The number of frames written in the current group, used to estimate frame duration.
	group_frames: u64,

	// When set, automatically manage group boundaries based on duration.
	max_group_duration: Option<Timestamp>,
}

impl OrderedProducer {
	/// Create a new OrderedProducer wrapping the given moq-lite producer.
	pub fn new(inner: moq_lite::TrackProducer) -> Self {
		Self {
			track: inner,
			group: None,
			group_start: None,
			group_frames: 0,
			max_group_duration: None,
		}
	}

	/// Set the maximum group duration for automatic group management.
	///
	/// Groups will be automatically closed and new ones started when the estimated
	/// next frame would exceed this duration.
	pub fn with_max_group_duration(mut self, duration: Timestamp) -> Self {
		self.max_group_duration = Some(duration);
		self
	}

	/// Signal that the next frame starts a new group (keyframe).
	///
	/// Finishes the current group if one exists. The next call to `write()`
	/// will create a new group.
	pub fn keyframe(&mut self) -> Result<(), Error> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
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

		// Safety check: close the group if this frame already exceeds the max duration.
		if let (Some(max_duration), Some(group_start)) = (self.max_group_duration, self.group_start)
			&& self.group.is_some()
			&& frame.timestamp.checked_sub(group_start).unwrap_or(Timestamp::ZERO) >= max_duration
			&& let Some(mut group) = self.group.take()
		{
			group.finish()?;
		}

		// Start a new group if needed (first frame, after keyframe(), or after auto-close).
		if self.group.is_none() {
			let group = self.track.append_group()?;
			self.group = Some(group);
			self.group_start = Some(frame.timestamp);
			self.group_frames = 0;
		}

		// Encode the frame.
		let mut group = self.group.take().expect("group should exist");
		frame.encode(&mut group)?;
		self.group.replace(group);

		self.group_frames += 1;

		// Estimate the next frame's timestamp and close the group now if it would exceed the limit.
		// avg_frame_duration = elapsed / group_frames
		// estimated_next_elapsed = elapsed + avg_frame_duration
		// Rearranged to avoid division: elapsed * (frames + 1) >= max_duration * frames
		if let (Some(max_duration), Some(group_start)) = (self.max_group_duration, self.group_start) {
			let elapsed = frame
				.timestamp
				.checked_sub(group_start)
				.unwrap_or(Timestamp::ZERO)
				.as_micros();
			let max = max_duration.as_micros();

			if elapsed * (self.group_frames as u128 + 1) >= max * self.group_frames as u128
				&& let Some(mut group) = self.group.take()
			{
				group.finish()?;
			}
		}

		Ok(())
	}

	/// Finish the producer, closing the current group and track.
	///
	/// After calling this, any further calls to `write()` will return an error.
	pub fn finish(&mut self) -> Result<(), Error> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
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
