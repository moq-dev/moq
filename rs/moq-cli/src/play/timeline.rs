//! Sample accounting for the player's speaker.

use std::time::Duration;

use hang::moq_net::Timestamp;

/// A wire timestamp as a duration from the start of the track.
pub(super) fn timestamp(timestamp: Timestamp) -> Duration {
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

/// How to hand a write to a speaker already holding `buffered`, so it sits on
/// `target`: the jitter buffer the browser's audio rings hold, in sample frames.
/// The write is `fill` samples of silence standing in for a hole in the media,
/// then the `frame` itself.
///
/// A `dry` ring, one that ran out or has only just opened, re-stalls: it pads up
/// to the target before playing on, rather than playing the next arrival on an
/// empty cushion. A ring more than `slack` over the target lands back on it by
/// skipping the oldest of what is arriving.
///
/// A playing ring is judged on what it holds ahead of the frame. The speaker
/// drains a frame before the next one is due, so counting it would skip a frame
/// that lands a little early and leave the ring a frame short for the next late
/// one: a click, then a dropout. The slack is what a device period moves the
/// level by, so ordinary cadence never skips.
pub(super) fn fit(dry: bool, buffered: u64, target: u64, slack: u64, fill: u64, frame: u64) -> Fit {
	let incoming = fill + frame;
	let pad = if dry {
		target.saturating_sub(buffered + incoming)
	} else {
		0
	};
	let level = buffered + pad + incoming;
	let ahead = if dry { level } else { level - frame };
	let skip = if ahead > target + slack {
		(ahead - target).min(incoming)
	} else {
		0
	};
	Fit { pad, skip }
}

/// What [`fit`] decided for one write.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Fit {
	/// Silence to write first.
	pub(super) pad: u64,
	/// Samples to drop from the front of the write.
	pub(super) skip: u64,
}

#[cfg(test)]
mod tests {
	use super::*;

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

	/// A dry ring refills to the target before the next arrival plays.
	#[test]
	fn a_dry_ring_restalls_to_the_target() {
		assert_eq!(fit(true, 0, 4_800, 960, 0, 960), Fit { pad: 3_840, skip: 0 });
		// A fresh sink starts on a little silence of its own, which counts.
		assert_eq!(fit(true, 2_400, 4_800, 960, 0, 960), Fit { pad: 1_440, skip: 0 });
		// A write longer than the target needs no padding, only the landing.
		assert_eq!(fit(true, 0, 960, 960, 0, 4_800), Fit { pad: 0, skip: 3_840 });
	}

	/// Ordinary cadence, a write on a ring at most the slack over the target, loses
	/// nothing, however long the write.
	#[test]
	fn cadence_within_the_slack_plays_everything() {
		assert_eq!(fit(false, 4_000, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
		assert_eq!(fit(false, 4_800, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
		// A frame that lands early, on a ring a device period over the target.
		assert_eq!(fit(false, 5_760, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
		assert_eq!(fit(false, 4_800, 4_800, 960, 0, 1_920), Fit { pad: 0, skip: 0 });
		// Running low is not running dry: nothing is padded until the ring is out.
		assert_eq!(fit(false, 480, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
	}

	/// A ring holding more than the slack over the target lands back on it,
	/// skipping as much of the write as that takes.
	#[test]
	fn excess_lands_on_the_target() {
		assert_eq!(fit(false, 5_761, 4_800, 960, 0, 960), Fit { pad: 0, skip: 960 });
		assert_eq!(fit(false, 6_000, 4_800, 960, 0, 2_400), Fit { pad: 0, skip: 1_200 });
		// A target that fell is reached by skipping, not by waiting to drain.
		assert_eq!(fit(false, 9_600, 4_800, 960, 0, 960), Fit { pad: 0, skip: 960 });
	}

	/// The silence standing in for a hole sits ahead of the frame, so it is what
	/// an over-full ring skips first; a ring the hole drained plays it through.
	#[test]
	fn a_hole_is_skipped_before_its_frame() {
		assert_eq!(fit(false, 4_800, 4_800, 960, 1_920, 960), Fit { pad: 0, skip: 1_920 });
		assert_eq!(fit(false, 4_000, 4_800, 960, 1_920, 960), Fit { pad: 0, skip: 1_120 });
		assert_eq!(fit(false, 2_880, 4_800, 960, 1_920, 960), Fit { pad: 0, skip: 0 });
	}
}
