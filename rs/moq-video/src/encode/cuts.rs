//! [`Cuts`]: which captured frames to force a keyframe on.

/// One encoder's view of the keyframe asks: decides which frames to cut, coalescing.
/// Built fresh for every encoder the capture opens.
///
/// Two asks open a group. `Control::cut` requests arrive as a running
/// count, so any number between two frames read as one. A subscriber joining over the
/// track's refresh ratio arrives as the group
/// [`Demand::wanted`](moq_net::track::Demand::wanted) asks for once it passes the largest
/// written, which only grows, so joins while the keyframe is still in the encoder ask for the
/// same group and coalesce into it.
///
/// There is no spacing between cuts: the refresh ratio already rate-limits joins by the bytes
/// a group holds, and a floor would let a group grow well past that ratio.
pub(super) struct Cuts {
	/// The request count already accounted for.
	requests: u64,
	/// The largest wanted group already cut for, or `None` before any.
	wanted: Option<u64>,
	/// Whether this encoder produced its first frame.
	opened: bool,
}

impl Cuts {
	/// Start from `requests`, the count already served by earlier encoders.
	pub fn new(requests: u64) -> Self {
		Self {
			requests,
			wanted: None,
			opened: false,
		}
	}

	/// Whether the next frame should be cut, given the running request count and `wanted`,
	/// the group demand asks for when it is past the largest written.
	pub fn due(&mut self, requests: u64, wanted: Option<u64>) -> bool {
		let asked = requests != self.requests || wanted > self.wanted;
		self.requests = requests;
		self.wanted = self.wanted.max(wanted);

		// A fresh encoder opens with a keyframe on every backend, which serves anything
		// asked for before it.
		if !self.opened {
			self.opened = true;
			return false;
		}
		asked
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_opening_keyframe_serves_earlier_asks() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(1, Some(4)));
		assert!(!cuts.due(1, Some(4)), "the asks were already served");
	}

	#[test]
	fn requests_before_a_frame_coalesce() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, None));
		assert!(cuts.due(10, None));
		assert!(!cuts.due(10, None), "ten requests before one frame cut once");
	}

	/// Back-to-back requests each cut: the caller decides how often to ask.
	#[test]
	fn requests_are_not_spaced() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, None));
		assert!(cuts.due(1, None));
		assert!(cuts.due(2, None), "a request one frame after a cut still cuts");
	}

	#[test]
	fn a_reopened_encoder_ignores_requests_already_counted() {
		let mut cuts = Cuts::new(5);
		assert!(!cuts.due(5, None));
		assert!(!cuts.due(5, None));
	}

	/// A join cuts once per wanted group: the frames still in the encoder ask for the same
	/// group, and a group that lands drops the ask until another join raises it.
	#[test]
	fn a_wanted_group_cuts_once() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, None));
		assert!(cuts.due(0, Some(1)), "a join past the largest group");
		assert!(!cuts.due(0, Some(1)), "the keyframe is still in the encoder");
		assert!(!cuts.due(0, None), "group 1 landed");
		assert!(cuts.due(0, Some(2)), "the next join");
	}

	/// A request and a join before the same frame cut it once.
	#[test]
	fn a_request_and_a_join_coalesce() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, None));
		assert!(cuts.due(1, Some(1)));
		assert!(!cuts.due(1, Some(1)));
	}
}
