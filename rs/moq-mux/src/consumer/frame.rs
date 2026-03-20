use buf_list::BufList;

use super::Timestamp;

/// A frame returned by [`super::OrderedConsumer::read()`] with group context.
#[derive(Clone, Debug)]
pub struct OrderedFrame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame, split into chunks.
	pub payload: BufList,

	/// The group sequence number this frame belongs to.
	pub group: u64,

	/// The frame index within the group (0 = first frame in the group).
	///
	/// With duration-based grouping (e.g. audio), the first frame is not
	/// necessarily a keyframe — it only denotes position within the group.
	pub index: usize,
}

impl OrderedFrame {
	/// Returns true if this is the first frame in the group (index 0).
	pub fn is_keyframe(&self) -> bool {
		self.index == 0
	}
}
