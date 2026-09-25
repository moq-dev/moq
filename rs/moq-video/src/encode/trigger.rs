//! [`Trigger`]: ask a running capture publish for a keyframe.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use moq_net::Timestamp;

/// The closest two forced keyframes may land, in media time.
///
/// A keyframe costs several times a predicted frame, so a caller asking in a loop
/// would otherwise pin the encoder at all-IDR and starve the rest of the uplink.
/// Well under the default two-second GOP, so a request still beats the cadence.
const MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Asks a running [`publish_capture`](super::publish_capture) for a keyframe.
///
/// Hand a clone to [`Options::trigger`](super::Options::trigger) and keep this one.
/// Each [`cut`](Self::cut) opens a new group at a frame no earlier than the call,
/// for a resume, a recording cut, or a known tune-in moment. Requests coalesce:
/// any number before the next frame produce one keyframe. The publisher also
/// spaces forced keyframes at least half a second apart, deferring a request
/// rather than dropping it.
///
/// A request while nothing is watching is served by the keyframe every fresh
/// encoder opens with. A backend that cannot force one logs a warning and keeps
/// its GOP cadence; see [`Error::CutUnsupported`](crate::Error::CutUnsupported).
#[derive(Clone, Debug, Default)]
pub struct Trigger(Arc<AtomicBool>);

impl Trigger {
	/// Request a keyframe at the next frame the publisher allows.
	pub fn cut(&self) {
		self.0.store(true, Ordering::Relaxed);
	}

	/// Consume an outstanding request.
	fn take(&self) -> bool {
		self.0.swap(false, Ordering::Relaxed)
	}
}

/// One encoder's view of a [`Trigger`]: decides which frames to cut, coalescing and
/// rate limiting the requests. Built fresh for every encoder the publisher opens.
pub(super) struct Cuts {
	trigger: Trigger,
	/// A request not yet honored because the last keyframe was too recent.
	pending: bool,
	/// The last keyframe this encoder was asked for, or `None` before its first frame.
	last: Option<Timestamp>,
}

impl Cuts {
	pub fn new(trigger: &Trigger) -> Self {
		Self {
			trigger: trigger.clone(),
			pending: false,
			last: None,
		}
	}

	/// Whether the frame at `timestamp` should be cut, recording it if so.
	pub fn due(&mut self, timestamp: Timestamp) -> bool {
		self.pending |= self.trigger.take();

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
		let trigger = Trigger::default();
		let mut cuts = Cuts::new(&trigger);
		trigger.cut();
		assert!(!cuts.due(at(0)));
		assert!(!cuts.due(at(1_000)), "the request was already served");
	}

	#[test]
	fn requests_before_a_frame_coalesce() {
		let trigger = Trigger::default();
		let mut cuts = Cuts::new(&trigger);
		assert!(!cuts.due(at(0)));

		for _ in 0..10 {
			trigger.cut();
		}
		assert!(cuts.due(at(1_000)));
		assert!(!cuts.due(at(2_000)), "ten requests before one frame cut once");
	}

	#[test]
	fn a_request_too_soon_is_deferred_not_dropped() {
		let trigger = Trigger::default();
		let mut cuts = Cuts::new(&trigger);
		assert!(!cuts.due(at(0)));

		trigger.cut();
		assert!(!cuts.due(at(100)));
		assert!(!cuts.due(at(499)));
		assert!(cuts.due(at(500)), "held until the interval elapsed");
	}

	#[test]
	fn a_caller_in_a_loop_cannot_force_all_idr() {
		let trigger = Trigger::default();
		let mut cuts = Cuts::new(&trigger);

		// Ten seconds at 30 fps with a request before every frame.
		let cut = (0..300u64)
			.filter(|frame| {
				trigger.cut();
				cuts.due(at(frame * 1000 / 30))
			})
			.count();
		assert_eq!(cut, 19, "one forced keyframe per half second after the opening one");
	}
}
