//! [`Cuts`]: which captured frames to force a keyframe on.

use std::time::Duration;

use moq_net::Timestamp;

/// The closest a forced keyframe may land after any other keyframe, in media time.
///
/// A keyframe costs several times a predicted frame, so a caller asking in a loop
/// would otherwise pin the encoder at all-IDR and starve the rest of the uplink.
/// Well under the default two-second GOP, so a request still beats the cadence.
const MIN_INTERVAL: Duration = Duration::from_millis(500);

/// One encoder's view of the keyframe requests: decides which frames to cut,
/// coalescing and rate limiting. Built fresh for every encoder the capture opens.
///
/// Requests arrive as a running count, so any number between two frames read as one.
/// Every keyframe the encoder produces counts, including its own GOP cadence: it
/// serves the requests made before its frame and restarts the spacing window.
pub(super) struct Cuts {
	/// The request count already accounted for.
	seen: u64,
	/// The frame the latest unserved request was noticed at, or `None` when none waits.
	/// A keyframe at or after it serves every request so far.
	pending: Option<Timestamp>,
	/// The latest keyframe, forced or not, or `None` before this encoder's first frame.
	last: Option<Timestamp>,
}

impl Cuts {
	/// Start from `requests`, the count already served by earlier encoders.
	pub fn new(requests: u64) -> Self {
		Self {
			seen: requests,
			pending: None,
			last: None,
		}
	}

	/// Whether the frame at `timestamp` should be cut, given the running request
	/// count, recording it if so.
	pub fn due(&mut self, requests: u64, timestamp: Timestamp) -> bool {
		if requests != self.seen {
			self.seen = requests;
			self.pending = Some(timestamp);
		}

		// A fresh encoder opens with a keyframe on every backend, which serves anything
		// requested before it.
		let Some(last) = self.last else {
			self.last = Some(timestamp);
			self.pending = None;
			return false;
		};

		if self.pending.is_none() || Duration::from(timestamp).saturating_sub(Duration::from(last)) < MIN_INTERVAL {
			return false;
		}

		self.pending = None;
		self.last = Some(timestamp);
		true
	}

	/// Record a keyframe the encoder produced for the frame at `timestamp`, whether
	/// forced or on its own cadence.
	pub fn keyframe(&mut self, timestamp: Timestamp) {
		// A buffering encoder can hand back a keyframe older than the latest request,
		// which is not a frame that request asked for.
		if self.pending.is_some_and(|pending| timestamp >= pending) {
			self.pending = None;
		}
		self.last = Some(self.last.map_or(timestamp, |last| last.max(timestamp)));
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

	/// The encoder's own GOP keyframe lands while a request waits out the spacing
	/// window, so the request is served and no extra keyframe is forced.
	#[test]
	fn a_cadence_keyframe_serves_a_waiting_request() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, at(0)));
		assert!(cuts.due(1, at(1_000)), "forced");
		cuts.keyframe(at(1_000));

		assert!(!cuts.due(2, at(1_100)), "too soon after the forced keyframe");
		assert!(!cuts.due(2, at(1_200)));
		cuts.keyframe(at(1_200));

		// Well past the window, and nothing left to force.
		assert!(!cuts.due(2, at(1_600)));
		assert!(!cuts.due(2, at(3_000)));
	}

	/// A cadence keyframe restarts the spacing window, so a request right after it
	/// cannot put two keyframes a frame apart.
	#[test]
	fn a_request_right_after_a_cadence_keyframe_waits_the_interval() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, at(0)));
		assert!(!cuts.due(0, at(2_000)));
		cuts.keyframe(at(2_000));

		assert!(!cuts.due(1, at(2_033)));
		assert!(!cuts.due(1, at(2_499)));
		assert!(cuts.due(1, at(2_500)), "held until the interval elapsed");
	}

	/// A buffering encoder can return a keyframe for a frame read before the
	/// request, which does not serve it.
	#[test]
	fn a_keyframe_older_than_the_request_does_not_serve_it() {
		let mut cuts = Cuts::new(0);
		assert!(!cuts.due(0, at(0)));
		assert!(!cuts.due(1, at(100)), "too soon");
		cuts.keyframe(at(50));

		assert!(!cuts.due(1, at(549)), "the window restarts at the latest keyframe");
		assert!(cuts.due(1, at(550)), "the request is still owed a keyframe");
	}
}
