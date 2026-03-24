use bytes::Bytes;

use crate::container::Timestamp;

/// A frame with keyframe context, used by [`Consumer`](super::Consumer) and [`Producer`](super::Producer).
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,

	/// Whether this frame is a keyframe (first frame in the group).
	pub keyframe: bool,
}

impl From<Frame> for crate::container::Frame {
	fn from(f: Frame) -> Self {
		crate::container::Frame {
			timestamp: f.timestamp,
			payload: f.payload,
		}
	}
}
