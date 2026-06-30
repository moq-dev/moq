//! Pace media output at its real-time (media-clock) rate.

use std::time::{Duration, Instant};

use super::Timestamp;

/// Maps frame timestamps onto the wall clock so output drains at the source's
/// real-time rate, like ffmpeg's `-re`.
///
/// The first frame seen anchors the media clock to "now"; every later frame is
/// due at `anchor + (timestamp - base)`. Sleep until [`Pacer::due`] before
/// emitting each frame and a retained broadcast plays out at its media rate
/// instead of as fast as it can be read. A live source is unaffected: its frames
/// already arrive paced, so each maps to roughly now.
///
/// The anchor never moves, so the media rate is held even when frames are read in
/// a burst. (Contrast the SRT egress stamper, which re-anchors to the live edge
/// because the receiver owns the jitter buffer.)
///
/// Drive it with an explicit `now` so the mapping stays testable and runtime-free:
/// the caller owns both the clock read and the sleep.
#[derive(Default)]
pub struct Pacer {
	anchor: Option<Anchor>,
}

/// The media-clock anchor: `base`'s media time maps to `at` on the wall clock.
struct Anchor {
	at: Instant,
	base: Timestamp,
}

impl Pacer {
	/// A pacer that anchors on the first frame it sees.
	pub fn new() -> Self {
		Self::default()
	}

	/// The wall-clock instant `timestamp` is due, given the current reading `now`.
	///
	/// The first call anchors `now` to `timestamp` and returns `now`. Later calls
	/// return `anchor + (timestamp - base)`: an instant that leads `now` for a
	/// faster-than-real source (sleep until it), or trails `now` for a frame that's
	/// already late, such as a reordered B-frame whose presentation timestamp dips
	/// below the anchor (emit it immediately).
	pub fn due(&mut self, timestamp: Timestamp, now: Instant) -> Instant {
		let anchor = self.anchor.get_or_insert(Anchor {
			at: now,
			base: timestamp,
		});

		match timestamp.checked_sub(anchor.base) {
			Ok(ahead) => anchor.at + Duration::from(ahead),
			Err(_) => anchor
				.at
				.checked_sub(Duration::from(anchor.base - timestamp))
				.unwrap_or(anchor.at),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ms(m: u64) -> Timestamp {
		Timestamp::from_micros(m * 1_000).unwrap()
	}

	#[test]
	fn anchors_first_frame_to_now() {
		let mut pacer = Pacer::new();
		let now = Instant::now();
		assert_eq!(pacer.due(ms(5_000), now), now, "the first frame is due immediately");
	}

	#[test]
	fn paces_on_the_media_clock() {
		let mut pacer = Pacer::new();
		let start = Instant::now();
		assert_eq!(pacer.due(ms(1_000), start), start);

		// A frame 40ms later in media is due 40ms after the anchor, regardless of how
		// quickly it was read (a retained broadcast hands frames over near-instantly).
		assert_eq!(
			pacer.due(ms(1_040), start + Duration::from_millis(1)),
			start + Duration::from_millis(40),
			"output is paced on the media clock, not arrival time"
		);
	}

	#[test]
	fn reordered_frame_is_already_due() {
		let mut pacer = Pacer::new();
		let start = Instant::now();
		pacer.due(ms(1_000), start);

		// A reordered B-frame whose PTS dips 33ms below the anchor maps into the past,
		// so the caller's sleep is a no-op and it's emitted immediately.
		assert_eq!(
			pacer.due(ms(967), start + Duration::from_millis(5)),
			start - Duration::from_millis(33),
		);
	}

	#[test]
	fn anchor_is_stable() {
		// Unlike the SRT live-edge stamper, the anchor never moves: pacing a retained
		// stream must hold the media rate, not collapse later frames onto the edge.
		let mut pacer = Pacer::new();
		let start = Instant::now();
		pacer.due(ms(0), start);
		assert_eq!(pacer.due(ms(10_000), start), start + Duration::from_secs(10));
		assert_eq!(pacer.due(ms(20), start), start + Duration::from_millis(20));
	}
}
