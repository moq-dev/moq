//! Media time: the presentation clock, and the audio track's own timeline.

use std::time::{Duration, Instant};

/// Where media time was at a known wall-clock instant, so presentation can
/// extrapolate from it rather than waking per frame.
#[derive(Clone, Copy)]
pub(super) struct Clock {
	pub(super) media: Duration,
	pub(super) wall: Instant,
}

impl Clock {
	pub(super) fn now(self) -> Duration {
		self.media.saturating_add(self.wall.elapsed())
	}
}

/// A wire timestamp as a duration from the start of the track.
pub(super) fn timestamp(timestamp: hang::moq_net::Timestamp) -> Duration {
	Duration::from_micros(timestamp.as_micros().min(u64::MAX as u128) as u64)
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clock_advances_from_its_media_anchor() {
		let clock = Clock {
			media: Duration::from_secs(10),
			wall: Instant::now() - Duration::from_millis(20),
		};
		assert!(clock.now() >= Duration::from_millis(10_020));
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
}
