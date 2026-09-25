//! [`Cuts`]: which captured frames to force a keyframe on.

use std::time::Duration;

use moq_net::Timestamp;

/// The closest two forced keyframes may land, in media time.
///
/// A keyframe costs several times a predicted frame, so a caller asking in a loop
/// would otherwise pin the encoder at all-IDR and starve the rest of the uplink.
/// Well under the default two-second GOP, so a request still beats the cadence.
const MIN_INTERVAL: Duration = Duration::from_millis(500);

/// One encoder's view of the keyframe requests: decides which frames to cut,
/// coalescing and rate limiting. Built fresh for every encoder the capture opens.
///
/// Requests arrive as a running count, so any number between two frames read as one.
pub(super) struct Cuts {
	/// The request count already accounted for.
	seen: u64,
	/// A request not yet honored because the last keyframe was too recent.
	pending: bool,
	/// The last keyframe this encoder was asked for, or `None` before its first frame.
	last: Option<Timestamp>,
}

impl Cuts {
	/// Start from `requests`, the count already served by earlier encoders.
	pub fn new(requests: u64) -> Self {
		Self {
			seen: requests,
			pending: false,
			last: None,
		}
	}

	/// Whether the frame at `timestamp` should be cut, given the running request
	/// count, recording it if so.
	pub fn due(&mut self, requests: u64, timestamp: Timestamp) -> bool {
		self.pending |= requests != self.seen;
		self.seen = requests;

		// A fresh encoder opens with a keyframe on every backend, which serves anything
		// requested before it.
		let Some(last) = self.last else {
			self.last = Some(timestamp);
			self.pending = false;
			return false;
		};

		if !self.pending || Duration::from(timestamp).saturating_sub(Duration::from(last)) < MIN_INTERVAL {
			return false;
		}

		self.pending = false;
		self.last = Some(timestamp);
		true
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn at(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	#[test]
	fn the_opening_keyframe_serves_earlier_requests() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(1, at(0)));
		assert!(!cuts.due(1, at(1_000)), "the request was already served");
	}

	#[test]
	fn requests_before_a_frame_coalesce() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, at(0)));
		assert!(cuts.due(10, at(1_000)));
		assert!(!cuts.due(10, at(2_000)), "ten requests before one frame cut once");
	}

	#[test]
	fn a_request_too_soon_is_deferred_not_dropped() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, at(0)));
		assert!(!cuts.due(1, at(100)));
		assert!(!cuts.due(1, at(499)));
		assert!(cuts.due(1, at(500)), "held until the interval elapsed");
	}

	#[test]
	fn a_caller_in_a_loop_cannot_force_all_idr() {
		let mut cuts = Cuts::new(0);

		// Ten seconds at 30 fps with a request before every frame.
		let cut = (0..300u64)
			.filter(|frame| cuts.due(frame + 1, at(frame * 1000 / 30)))
			.count();
		assert_eq!(cut, 19, "one forced keyframe per half second after the opening one");
	}

	#[test]
	fn a_reopened_encoder_ignores_requests_already_counted() {
		let mut cuts = Cuts::new(5);
		assert!(!cuts.due(5, at(0)));
		assert!(!cuts.due(5, at(1_000)));
	}
}
