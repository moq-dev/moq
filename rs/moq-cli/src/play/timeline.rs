//! Media time: the playout clock, and the audio track's own timeline.

use std::time::{Duration, Instant};

use hang::moq_net::Timestamp;

/// The playout clock: one anchor the window and the speaker both present
/// against.
///
/// Every arriving frame is folded into a single [`Pacer`](moq_mux::Pacer), which
/// maps media time onto the wall clock and holds each result back by the
/// configured delay. That is what pulls playback toward live: a frame arriving
/// earlier than the anchor predicted re-pins it, and the delay is measured from
/// the edge it just set rather than from wherever the first frame happened to
/// land.
///
/// While audio is playing it is the only half allowed to move the anchor. The
/// speaker drains on its own clock and cannot skip forward with a re-anchor, so
/// a video tune-in burst re-anchoring would leave the picture ahead of the
/// sound. Video reads the anchor and follows it.
pub(super) struct Presentation {
	pacer: moq_mux::Pacer,
	/// How far behind the live edge playback runs.
	delay: Duration,
	/// Whether the speaker owns the anchor.
	speaker: bool,
	/// Set when the speaker restarts on a new timeline, so the next audio frame
	/// pins the anchor outright instead of pacing against media it will never
	/// play.
	restart: bool,
}

impl Presentation {
	pub(super) fn new(delay: Duration) -> Self {
		Self {
			pacer: moq_mux::Pacer::default().with_delay(delay),
			delay,
			speaker: false,
			restart: false,
		}
	}

	/// Fold an arriving video frame into the anchor, unless the speaker owns it.
	pub(super) fn video(&mut self, timestamp: Timestamp, now: Instant) {
		if !self.speaker {
			self.pacer.pace(timestamp, now);
		}
	}

	/// Fold the speaker's position into the anchor, reporting whether that moved
	/// the schedule.
	///
	/// `end` is the media time of the last sample written and `buffered` is how
	/// much of the write is still queued, so `end` does not sound until
	/// `buffered` from now. The anchor is pinned at the instant that leaves
	/// exactly the delay before it does, which is where the live edge sits on a
	/// clock trailing by that delay. Pinning the sample sounding *now* instead
	/// would schedule video a whole delay behind the speaker.
	///
	/// A move makes every queued video frame due earlier, so the window has to
	/// recompute the deadline it is already asleep on.
	pub(super) fn audio(&mut self, end: Duration, buffered: Duration, now: Instant) -> bool {
		let timestamp = media(end);
		let edge = now
			.checked_add(buffered)
			.and_then(|at| at.checked_sub(self.delay))
			.unwrap_or(now);

		let before = self.pacer.due(timestamp);
		let after = if std::mem::take(&mut self.restart) {
			self.pacer.hurry(timestamp, edge)
		} else {
			self.pacer.pace(timestamp, edge)
		};
		self.speaker = true;

		before != Some(after)
	}

	/// The speaker restarted on a new timeline, so the media either side of the
	/// break is unrelated: re-pin the anchor on the next audio frame rather than
	/// pacing across a jump the speaker never plays.
	pub(super) fn restarted(&mut self) {
		self.restart = true;
	}

	/// The audio track stopped, so nothing holds playback to the speaker's
	/// cadence any more and video takes the anchor back.
	pub(super) fn stopped(&mut self) {
		self.speaker = false;
	}

	/// When `timestamp` should be presented, or `None` before any frame has
	/// anchored the clock. `None` means as soon as possible: there is nothing
	/// left to wait for.
	pub(super) fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.pacer.due(timestamp)
	}
}

/// A wire timestamp as a duration from the start of the track.
pub(super) fn timestamp(timestamp: Timestamp) -> Duration {
	Duration::from_micros(timestamp.as_micros().min(u64::MAX as u128) as u64)
}

/// A duration from the start of the track as a wire timestamp, the inverse of
/// [`timestamp`].
fn media(duration: Duration) -> Timestamp {
	// A wire timestamp caps at 2^62 - 1, which is ~146,000 years of microseconds.
	const MAX: u128 = (1 << 62) - 1;
	Timestamp::from_micros(duration.as_micros().min(MAX) as u64).expect("clamped to the wire maximum")
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

	const DELAY: Duration = Duration::from_millis(100);

	fn ms(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	/// The defect this clock exists for: a first frame that arrives late used to
	/// pin the anchor once and leave playback that far behind live for the whole
	/// session. Every earlier arrival has to pull it forward instead, without
	/// giving up the delay.
	#[test]
	fn a_late_first_frame_catches_up_to_live() {
		let start = Instant::now();
		let mut presentation = Presentation::new(DELAY);

		// Produced 500ms ago, though nothing here knows that yet.
		presentation.video(ms(0), start);
		assert_eq!(presentation.due(ms(0)), Some(start + DELAY));

		// 40ms of media later, but only 20ms of wall clock: the anchor was 480ms
		// behind live, so it moves onto this frame.
		let now = start + Duration::from_millis(20);
		presentation.video(ms(40), now);
		assert_eq!(presentation.due(ms(40)), Some(now + DELAY));

		// A frame that merely arrives late keeps its media instant. The delay is
		// the room it has to be late in.
		let late = now + Duration::from_millis(70);
		presentation.video(ms(80), late);
		assert_eq!(presentation.due(ms(80)), Some(now + Duration::from_millis(140)));
	}

	/// The speaker cannot skip forward with a re-anchor, so while it is playing a
	/// video burst must not move the anchor out from under it.
	#[test]
	fn the_speaker_owns_the_anchor_while_it_plays() {
		let start = Instant::now();
		let mut presentation = Presentation::new(DELAY);

		presentation.audio(Duration::from_secs(1), DELAY, start);
		let anchored = presentation.due(ms(1_000));

		// A tune-in burst: seconds of video arriving at once.
		presentation.video(ms(1_040), start);
		presentation.video(ms(4_000), start);
		assert_eq!(
			presentation.due(ms(1_000)),
			anchored,
			"video moved the speaker's anchor"
		);

		// Once audio stops, video anchors again.
		presentation.stopped();
		presentation.video(ms(4_040), start);
		assert_eq!(presentation.due(ms(4_040)), Some(start + DELAY));
	}

	/// The speaker reports the sample sounding now, which is a delay behind the
	/// edge. Pinning that instant directly would schedule video a second delay
	/// behind the sound.
	#[test]
	fn the_speaker_anchors_at_the_live_edge() {
		let start = Instant::now();
		let mut presentation = Presentation::new(DELAY);

		// The last sample written is a full delay from sounding, which is exactly
		// where a settled sink sits: media time and the picture agree.
		presentation.audio(Duration::from_secs(1), DELAY, start);
		assert_eq!(presentation.due(ms(1_000)), Some(start + DELAY));

		// Filling up, with only 40ms queued: that sample sounds in 40ms, so the
		// picture that goes with it is due then too.
		let now = start + Duration::from_millis(500);
		presentation.audio(Duration::from_millis(1_500), Duration::from_millis(40), now);
		assert_eq!(presentation.due(ms(1_500)), Some(now + Duration::from_millis(40)));
	}

	/// A hole too large to play through drops the buffered audio and starts a new
	/// sink, and a rewind restarts the timeline outright. Pacing across either
	/// would schedule video against samples the speaker never plays.
	#[test]
	fn a_restarted_speaker_re_anchors() {
		let start = Instant::now();
		let mut presentation = Presentation::new(DELAY);
		presentation.audio(Duration::from_secs(10), DELAY, start);

		// The publisher rewound: without the restart this pins media 9 seconds
		// behind the anchor, leaving the picture 9 seconds in the past.
		let now = start + Duration::from_millis(20);
		presentation.restarted();
		presentation.audio(Duration::from_secs(1), DELAY, now);
		assert_eq!(presentation.due(ms(1_000)), Some(now + DELAY));
	}

	/// The window sleeps on the deadline it last computed, so an anchor the
	/// speaker moved has to say so or the queue presents late by that much.
	#[test]
	fn a_moved_anchor_is_reported() {
		let start = Instant::now();
		let mut presentation = Presentation::new(DELAY);
		assert!(
			presentation.audio(Duration::from_secs(1), DELAY, start),
			"the first anchor"
		);

		// On schedule: 40ms of media, 40ms of wall clock, same sink depth.
		let now = start + Duration::from_millis(40);
		assert!(
			!presentation.audio(Duration::from_millis(1_040), DELAY, now),
			"a frame on the anchor moved it"
		);

		// Arriving early re-anchors, and the window has to hear about it.
		assert!(
			presentation.audio(Duration::from_millis(1_100), DELAY, now),
			"an early frame left the anchor alone"
		);
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
