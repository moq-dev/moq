//! Sample accounting for the player's speaker.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kio::clock::Instant;

/// The media time the speaker is sounding, which `--metrics` reports.
///
/// The speaker converges on the shared clock rather than owning it, so where it
/// actually is stays its own: it runs dry in a stall and skips ahead to catch up.
///
/// Clones write one broadcast's speaker; [`reset`](Self::reset) starts the next
/// broadcast's, which readers follow while the old one's late writes go nowhere.
#[derive(Clone)]
pub(super) struct Playhead {
	own: Arc<Mutex<Written>>,
	current: Arc<Mutex<Arc<Mutex<Written>>>>,
}

impl Default for Playhead {
	fn default() -> Self {
		let own = Arc::new(Mutex::new(Written::default()));
		Self {
			current: Arc::new(Mutex::new(own.clone())),
			own,
		}
	}
}

#[derive(Default)]
struct Written {
	/// The last write: the media time it ends at, when that sounds, and the
	/// playhead it may not fall back behind on the same timeline.
	last: Option<(Duration, Instant, Option<Duration>)>,
	/// The next write starts a new timeline.
	restart: bool,
}

impl Playhead {
	/// Start the next broadcast's playhead and switch readers to it.
	#[must_use = "use the returned playhead for the replacement broadcast"]
	pub(super) fn reset(self) -> Self {
		let own = Arc::new(Mutex::new(Written::default()));
		*self.current.lock().unwrap() = own.clone();
		Self {
			own,
			current: self.current,
		}
	}

	/// Record a write ending at media `end`, of which `buffered` is still queued at `now`.
	pub(super) fn wrote(&self, end: Duration, buffered: Duration, now: Instant) {
		let mut written = self.own.lock().unwrap();
		// Silence padded ahead of the frame to refill the speaker is not media: the
		// playhead holds where it was until the padding has played, rather than
		// projecting back over media already heard. A new timeline starts afresh.
		let floor = if std::mem::take(&mut written.restart) {
			None
		} else {
			written.playhead(now)
		};
		written.last = now.checked_add(buffered).map(|sounds| (end, sounds, floor));
	}

	/// The speaker moved onto an unrelated timeline: a fresh sink or rendition.
	pub(super) fn restart(&self) {
		self.own.lock().unwrap().restart = true;
	}

	/// The speaker went quiet.
	pub(super) fn clear(&self) {
		*self.own.lock().unwrap() = Written::default();
	}

	/// The media time sounding at `now`, or `None` with no speaker.
	pub(super) fn at(&self, now: Instant) -> Option<Duration> {
		self.current.lock().unwrap().lock().unwrap().playhead(now)
	}
}

impl Written {
	/// Projected from the last write at the wall rate, held at its end (a speaker
	/// with nothing left plays nothing further), and never behind where it stood
	/// at that write (refill padding plays no media).
	fn playhead(&self, now: Instant) -> Option<Duration> {
		let (end, sounds, floor) = self.last?;
		let projected = end.saturating_sub(sounds.saturating_duration_since(now));
		Some(floor.map_or(projected, |floor| projected.max(floor)).min(end))
	}
}

/// Where the audio track has reached, measured from its own origin so
/// timestamp rounding can't accumulate into drift.
#[derive(Default)]
pub(super) struct AudioTimeline {
	origin: Option<Duration>,
	end: Option<Duration>,
	written: u64,
}

/// What the speaker owes before the frame just pushed: silence to play a hole
/// through, or a fresh sink when the timeline jumped too far to fill.
pub(super) struct AudioTiming {
	/// Media time the pushed frame ends at.
	pub(super) end: Duration,
	/// Samples of silence to write first.
	pub(super) silence: u64,
	/// Whether the buffered sink has to be replaced.
	pub(super) reset_sink: bool,
}

impl AudioTimeline {
	pub(super) fn push(&mut self, start: Duration, samples: usize, sample_rate: u32, fill_max: u64) -> AudioTiming {
		let duration = Duration::from_secs_f64(samples as f64 / sample_rate as f64);
		let end = start.saturating_add(duration);
		// Millisecond-stamped input can put adjacent frames on either side of their
		// exact boundary. Two output samples cover the conversions on top of that.
		let tolerance = Duration::from_millis(1).saturating_add(Duration::from_secs_f64(2.0 / sample_rate as f64));
		let rewound = self
			.end
			.is_some_and(|previous| start.saturating_add(tolerance) < previous);
		if rewound {
			self.origin = None;
			self.written = 0;
		}

		// Measure every hole from the track origin so timestamp rounding cannot
		// accumulate into drift. Advancing to `expected` even when the hole is skipped
		// keeps the next frame contiguous with the new timeline position.
		let origin = *self.origin.get_or_insert(start);
		let expected = (start.saturating_sub(origin).as_secs_f64() * sample_rate as f64).round() as u64;
		let hole = expected.saturating_sub(self.written);
		let skipped = hole > fill_max;
		let silence = if skipped { 0 } else { hole };
		let reset_sink = rewound || skipped;
		self.written = self
			.written
			.max(expected)
			.saturating_add(u64::try_from(samples).unwrap_or(u64::MAX));
		self.end = Some(end);

		AudioTiming {
			end,
			silence,
			reset_sink,
		}
	}
}

/// Keep the silence before a frame unless the distance is an emergency.
/// `buffered` excludes this frame, so normal packet cadence never skips it.
pub(super) fn fit(buffered: u64, target: u64, slack: u64, silence: u64, frame: u64) -> Fit {
	let ahead = buffered + silence;
	let skip = if ahead >= 4 * (target + slack) {
		(ahead - target).min(silence + frame)
	} else {
		0
	};
	Fit {
		silence: silence.saturating_sub(skip),
		skip: skip.saturating_sub(silence),
	}
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Fit {
	pub silence: u64,
	pub skip: u64,
}

#[cfg(test)]
mod tests {
	use super::*;

	const DELAY: Duration = Duration::from_millis(100);

	/// The playhead runs at the wall rate from the last write and stops at its
	/// end, which is what a speaker that ran dry is still sounding.
	#[test]
	fn the_playhead_projects_the_last_write_and_holds_at_its_end() {
		let now = Instant::ZERO + Duration::from_secs(1);
		let playhead = Playhead::default();
		assert_eq!(playhead.at(now), None);

		playhead.wrote(Duration::from_secs(10), Duration::from_millis(200), now);
		assert_eq!(playhead.at(now), Some(Duration::from_millis(9_800)));
		let later = now + Duration::from_millis(150);
		assert_eq!(playhead.at(later), Some(Duration::from_millis(9_950)));
		let dry = now + Duration::from_secs(1);
		assert_eq!(playhead.at(dry), Some(Duration::from_secs(10)));

		playhead.clear();
		assert_eq!(playhead.at(dry), None);
	}

	/// A replaced broadcast's late writes never reach the reader.
	#[test]
	fn a_reset_playhead_isolates_the_old_broadcast() {
		let now = Instant::ZERO + Duration::from_secs(1);
		let reader = Playhead::default();
		let old = reader.clone();
		let new = reader.clone().reset();
		old.wrote(Duration::from_secs(10), DELAY, now);
		assert_eq!(reader.at(now), None);
		new.wrote(Duration::from_secs(2), DELAY, now);
		assert_eq!(reader.at(now), Some(Duration::from_millis(1_900)));
	}

	/// Silence padded ahead of a frame after the speaker ran dry plays no media,
	/// so the playhead holds instead of replaying media already heard.
	#[test]
	fn the_playhead_holds_through_refill_padding() {
		let now = Instant::ZERO + Duration::from_secs(1);
		let playhead = Playhead::default();
		playhead.wrote(Duration::from_secs(10), Duration::from_millis(200), now);
		let dry = now + Duration::from_secs(1);
		assert_eq!(playhead.at(dry), Some(Duration::from_secs(10)));

		// 100 ms of padding, then a 20 ms frame continuing from 10 s.
		playhead.wrote(Duration::from_millis(10_020), Duration::from_millis(120), dry);
		assert_eq!(playhead.at(dry), Some(Duration::from_secs(10)));
		let padded = dry + Duration::from_millis(100);
		assert_eq!(playhead.at(padded), Some(Duration::from_secs(10)));
		let played = dry + Duration::from_millis(110);
		assert_eq!(playhead.at(played), Some(Duration::from_millis(10_010)));

		// A new timeline may start anywhere.
		playhead.restart();
		playhead.wrote(Duration::from_secs(2), DELAY, played);
		assert_eq!(playhead.at(played), Some(Duration::from_millis(1_900)));
	}

	#[test]
	fn audio_timeline_restarts_when_media_time_rewinds() {
		let mut timeline = AudioTimeline::default();
		let first = timeline.push(Duration::from_secs(10), 960, 48_000, 24_000);
		assert!(!first.reset_sink);

		let rewound = timeline.push(Duration::from_secs(5), 960, 48_000, 24_000);
		assert!(rewound.reset_sink);
		assert_eq!(rewound.silence, 0);

		let next = timeline.push(Duration::from_millis(5_020), 960, 48_000, 24_000);
		assert!(!next.reset_sink);
		assert_eq!(next.silence, 0);
	}

	#[test]
	fn audio_timeline_tolerates_millisecond_stamp_rounding() {
		let mut timeline = AudioTimeline::default();
		let first = timeline.push(Duration::ZERO, 1024, 44_100, 22_050);
		assert!(!first.reset_sink);

		// 1024 frames end at 23.22 ms, but an FLV timestamp carries 23 ms.
		let rounded = timeline.push(Duration::from_millis(23), 1024, 44_100, 22_050);
		assert!(!rounded.reset_sink);
	}

	#[test]
	fn audio_timeline_resets_sink_when_forward_hole_exceeds_fill_cap() {
		let mut timeline = AudioTimeline::default();
		timeline.push(Duration::ZERO, 960, 48_000, 4_800);

		let filled = timeline.push(Duration::from_millis(100), 960, 48_000, 4_800);
		assert!(!filled.reset_sink);
		assert_eq!(filled.silence, 3_840);

		let skipped = timeline.push(Duration::from_secs(1), 960, 48_000, 4_800);
		assert!(skipped.reset_sink);
		assert_eq!(skipped.silence, 0);

		let next = timeline.push(Duration::from_millis(1_020), 960, 48_000, 4_800);
		assert!(!next.reset_sink);
		assert_eq!(next.silence, 0);
	}

	#[test]
	fn emergency_skip_consumes_hole_silence_before_frame_audio() {
		assert_eq!(fit(23_039, 4_800, 960, 0, 960), Fit { silence: 0, skip: 0 });
		assert_eq!(fit(23_040, 4_800, 960, 0, 960), Fit { silence: 0, skip: 960 });
		assert_eq!(
			fit(6_000, 4_800, 960, 18_000, 2_400),
			Fit {
				silence: 0,
				skip: 1_200
			}
		);
		assert_eq!(
			fit(1_000, 4_800, 960, 23_040, 960),
			Fit {
				silence: 3_800,
				skip: 0
			}
		);
	}
}
