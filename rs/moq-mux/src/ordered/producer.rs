use crate::container::{Container, Frame};

/// A producer for media tracks that manages group boundaries based on keyframes.
///
/// Generic over `C: Container` to support different container encodings.
/// Creates a new group automatically when writing a keyframe.
///
/// ## Latency Buffering
///
/// When `latency` is zero (default), each frame is written immediately as its own
/// moq-lite frame. When non-zero, frames are buffered and flushed together when:
/// - A keyframe arrives (flushes the previous group's buffer, starts new group)
/// - The buffered duration exceeds `latency`
/// - `finish()` is called
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
	/// If the frame is a keyframe, a new group is created automatically.
	/// The first frame written must be a keyframe.
	pub fn write(&mut self, frame: Frame) -> Result<(), C::Error> {
		if frame.keyframe {
			// Flush any buffered frames from the previous group.
			self.flush()?;

			// Close the previous group.
			if let Some(mut group) = self.group.take() {
				group.finish()?;
			}

			// Start a new group.
			let group = self.track.append_group()?;
			self.group = Some(group);
		}

		if self.group.is_none() {
			return Err(moq_lite::Error::ProtocolViolation.into());
		}

		if self.latency.is_zero() {
			// Flush immediately.
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

	/// Flush any buffered frames to the container.
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
		self.flush()?;
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
