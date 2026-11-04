//! [`Cuts`]: which captured frames to force a keyframe on.

/// One encoder's view of the keyframe asks: decides which frames to cut, coalescing.
/// Built fresh for every encoder the capture opens.
///
/// A subscriber joining over the track's refresh ratio asks for the group
/// [`Demand::wanted`](moq_net::track::Demand::wanted) names once it passes the largest
/// written, which only grows, so joins while the keyframe is still in the encoder ask for the
/// same group and coalesce into it.
///
/// There is no spacing between cuts: the refresh ratio already rate-limits joins by the bytes
/// a group holds, and a floor would let a group grow well past that ratio.
#[derive(Default)]
pub(super) struct Cuts {
	/// The largest wanted group already cut for, or `None` before any.
	wanted: Option<u64>,
	/// Whether this encoder produced its first frame.
	opened: bool,
}

impl Cuts {
	/// Whether the next frame should be cut, given `wanted`, the group demand asks for when it
	/// is past the largest written.
	pub fn due(&mut self, wanted: Option<u64>) -> bool {
		let asked = wanted > self.wanted;
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
		let mut cuts = Cuts::default();
		assert!(!cuts.due(Some(4)));
		assert!(!cuts.due(Some(4)), "the ask was already served");
	}

	/// A join cuts once per wanted group: the frames still in the encoder ask for the same
	/// group, and a group that lands drops the ask until another join raises it.
	#[test]
	fn a_wanted_group_cuts_once() {
		let mut cuts = Cuts::default();
		assert!(!cuts.due(None));
		assert!(cuts.due(Some(1)), "a join past the largest group");
		assert!(!cuts.due(Some(1)), "the keyframe is still in the encoder");
		assert!(!cuts.due(None), "group 1 landed");
		assert!(cuts.due(Some(2)), "the next join");
	}

	/// Back-to-back joins each cut: the refresh ratio decides how often they arrive.
	#[test]
	fn joins_are_not_spaced() {
		let mut cuts = Cuts::default();
		assert!(!cuts.due(None));
		assert!(cuts.due(Some(1)));
		assert!(cuts.due(Some(2)), "a join one frame after a cut still cuts");
	}
}
