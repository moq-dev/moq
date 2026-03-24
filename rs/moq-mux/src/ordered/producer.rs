use super::Frame;
use crate::container::Container;

/// A producer for media tracks that manages group boundaries based on keyframes.
///
/// Generic over `C: Container` to support different container encodings.
/// Creates a new group automatically when writing a keyframe.
pub struct Producer<C: Container> {
	pub track: moq_lite::TrackProducer,
	container: C,
	group: Option<moq_lite::GroupProducer>,
}

impl<C: Container> Producer<C> {
	/// Create a new Producer wrapping the given moq-lite producer.
	pub fn new(track: moq_lite::TrackProducer, container: C) -> Self {
		Self {
			track,
			container,
			group: None,
		}
	}

	/// Write a frame to the track.
	///
	/// If the frame is a keyframe, a new group is created automatically.
	/// The first frame written must be a keyframe.
	pub fn write(&mut self, frame: Frame) -> Result<(), C::Error> {
		if frame.keyframe {
			if let Some(mut group) = self.group.take() {
				group.finish()?;
			}
		}

		let group = match &mut self.group {
			Some(group) => group,
			None => {
				if !frame.keyframe {
					return Err(moq_lite::Error::Decode.into());
				}
				let group = self.track.append_group()?;
				self.group.insert(group)
			}
		};

		self.container.write(group, &frame.into())
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<(), C::Error> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
	}

	/// Create a consumer for this track.
	pub fn consume(&self) -> moq_lite::TrackConsumer {
		self.track.consume()
	}
}
