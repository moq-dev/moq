use std::collections::VecDeque;

use crate::model::{Frame, Timestamp};
use crate::Result;

use buf_list::BufList;
use moq_lite::coding::Decode;
use moq_lite::lite;

/// A consumer for a group of frames.
///
/// Groups represent collections of frames that belong together, typically
/// bounded by keyframes in video streams. The first frame in a group is
/// automatically marked as a keyframe.
///
/// This wraps a `moq_lite::GroupConsumer` and provides hang-specific functionality:
/// - Timestamp decoding from frame headers.
/// - Keyframe detection based on frame position
/// - Frame buffering for latency management
/// - Maximum timestamp tracking for group boundaries
pub struct GroupConsumer {
	// The group.
	group: moq_lite::GroupConsumer,

	// The current frame index
	index: usize,

	// The any buffered frames in the group.
	buffered: VecDeque<Frame>,

	// The max timestamp in the group
	max_timestamp: Option<Timestamp>,
}

impl GroupConsumer {
	/// Create a new group consumer from a MoQ group consumer.
	pub fn new(group: moq_lite::GroupConsumer) -> Self {
		Self {
			group,
			index: 0,
			buffered: VecDeque::new(),
			max_timestamp: None,
		}
	}

	/// Read the next frame from the group.
	///
	/// This method automatically:
	/// - Decodes timestamps from frame headers
	/// - Marks the first frame as a keyframe
	/// - Returns buffered frames when available
	/// - Tracks the maximum timestamp seen
	///
	/// Returns `None` when the group has ended.
	pub async fn read(&mut self) -> Result<Option<Frame>> {
		if let Some(frame) = self.buffered.pop_front() {
			Ok(Some(frame))
		} else {
			self.read_unbuffered().await
		}
	}

	async fn read_unbuffered(&mut self) -> Result<Option<Frame>> {
		let payload = match self.group.next_frame().await? {
			Some(mut frame) => frame.read_chunks().await?,
			None => return Ok(None),
		};

		let mut payload = BufList::from_iter(payload);

		let micros = u64::decode(&mut payload, lite::Version::Draft02)?;
		let timestamp = Timestamp::from_micros(micros)?;

		let frame = Frame {
			keyframe: (self.index == 0),
			timestamp,
			payload,
		};

		self.index += 1;
		self.max_timestamp = Some(self.max_timestamp.unwrap_or_default().max(timestamp));

		Ok(Some(frame))
	}

	// Keep reading and buffering new frames, returning when `max` is larger than or equal to the cutoff.
	// Not publish because the API is super weird.
	// This will BLOCK FOREVER if the group has ended early; it's intended to be used within select!
	pub(super) async fn buffer_until(&mut self, cutoff: Timestamp) -> Timestamp {
		loop {
			match self.max_timestamp {
				Some(timestamp) if timestamp >= cutoff => return timestamp,
				_ => (),
			}

			match self.read().await {
				Ok(Some(frame)) => self.buffered.push_back(frame),
				// Otherwise block forever so we don't return from FuturesUnordered
				_ => std::future::pending().await,
			}
		}
	}

	/// Get the maximum timestamp seen in this group so far.
	///
	/// Returns `None` if no frames have been read yet.
	pub fn max_timestamp(&self) -> Option<Timestamp> {
		self.max_timestamp
	}
}

impl std::ops::Deref for GroupConsumer {
	type Target = moq_lite::GroupConsumer;

	fn deref(&self) -> &Self::Target {
		&self.group
	}
}
