use bytes::Bytes;

use crate::container::Timestamp;

/// A frame returned by [`super::OrderedConsumer::read()`] with keyframe context.
#[derive(Clone, Debug)]
pub struct OrderedFrame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,

	/// Whether this frame is a keyframe (first frame in the group).
	pub keyframe: bool,
}
