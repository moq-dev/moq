//! Picks which of a min-delay and a max-delay decoder's pictures to show.
//!
//! An intra-refresh rendition has no keyframes, so a decoder that joins or skips
//! mid-sweep shows refresh stripes until a whole sweep has passed. The min-delay
//! decoder keeps the caller's latency and may show them; the max-delay decoder
//! tolerates one more sweep of delay, so it keeps continuity. Both race on the
//! same frames in steady state, and taking whichever is newest would flicker
//! stripes between clean pictures, so the source is sticky instead.

use std::collections::VecDeque;
use std::time::Duration;

use moq_clock::Instant;

use crate::Frame;

/// One of the two decoders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lane {
	Min = 0,
	Max = 1,
}

/// Decoded pictures a lane may hold before its decoder waits for the selector.
///
/// Two, so a held min-delay picture still lets the decoder behind it move the
/// presentation clock the hold is measured on, without parking a backlog of
/// decoded surfaces a hardware pool may need back.
const QUEUE: usize = 2;

/// What [`Selector::next`] decided.
pub(super) enum Next {
	Frame(Frame),
	/// Nothing to show until a lane produces more, or until the deadline passes.
	Wait(Option<Instant>),
	End,
}

#[derive(Default)]
struct State {
	queue: VecDeque<(Frame, Instant)>,
	/// The newest timestamp this lane has decoded, shown or not.
	newest: Option<Duration>,
	/// Ended, or never opened.
	done: bool,
}

impl State {
	/// Done, with nothing left to show either.
	fn exhausted(&self) -> bool {
		self.done && self.queue.is_empty()
	}
}

pub(super) struct Selector {
	lanes: [State; 2],
	source: Lane,
	/// Whether there are two lanes to choose between.
	dual: bool,
	/// The last timestamp returned with two lanes, which output strictly
	/// increases past. A lone decoder's pictures pass through as they come.
	last: Option<Duration>,
	/// The catalog's frame interval, or the smallest step the min-delay lane has
	/// decoded. The smallest rather than the latest, so a skip never stretches it.
	interval: Option<Duration>,
	measured: bool,
}

impl Selector {
	/// A selector for one lane, or for both when `dual`.
	pub(super) fn new(dual: bool, interval: Option<Duration>) -> Self {
		let mut lanes: [State; 2] = Default::default();
		lanes[Lane::Max as usize].done = !dual;
		Self {
			lanes,
			source: Lane::Min,
			dual,
			last: None,
			measured: interval.is_none(),
			interval,
		}
	}

	/// The lane the last picture came from, or will next.
	#[cfg(test)]
	pub(super) fn source(&self) -> Lane {
		self.source
	}

	/// Whether `lane` can take another decoded picture.
	pub(super) fn wants(&self, lane: Lane) -> bool {
		let state = &self.lanes[lane as usize];
		!state.done && state.queue.len() < QUEUE
	}

	/// A picture `lane` decoded at `now`.
	pub(super) fn push(&mut self, lane: Lane, frame: Frame, now: Instant) {
		let timestamp = Duration::from(frame.timestamp);
		let state = &mut self.lanes[lane as usize];
		if lane == Lane::Min
			&& self.measured
			&& let Some(step) = state.newest.and_then(|newest| timestamp.checked_sub(newest))
			&& !step.is_zero()
		{
			self.interval = Some(self.interval.map_or(step, |interval| interval.min(step)));
		}
		state.newest = Some(state.newest.map_or(timestamp, |newest| newest.max(timestamp)));
		state.queue.push_back((frame, now));
	}

	/// `lane` ended and will decode nothing more.
	pub(super) fn finish(&mut self, lane: Lane) {
		self.lanes[lane as usize].done = true;
	}

	/// The next picture to return at `now`, or what to wait for.
	pub(super) fn next(&mut self, now: Instant) -> Next {
		// Nothing at or below the last picture returned is ever shown, so a
		// cutover never repeats or rewinds a picture.
		if let Some(last) = self.last {
			for state in &mut self.lanes {
				state.queue.retain(|(frame, _)| Duration::from(frame.timestamp) > last);
			}
		}

		let [min, max] = &self.lanes;
		let interval = self.interval.unwrap_or_default();

		if self.source == Lane::Min {
			// Max-delay pictures take over once that decoder has caught up to a
			// min-delay picture, not merely decoded first: on a join it starts a
			// sweep behind.
			let caught_up = !max.exhausted() && min.newest.is_some() && max.newest >= min.newest;
			if caught_up || (min.exhausted() && !max.exhausted()) {
				self.source = Lane::Max;
			}
		} else if let Some((held, at)) = min.queue.front() {
			// A newer min-delay picture waits for the max-delay decoder to reach
			// it, but never for longer than a frame: not when it already leads by
			// more than that, not once a frame's worth of wall or media time has
			// passed, and not after the max-delay decoder has nothing left.
			let held = Duration::from(held.timestamp);
			let leads = max.newest.is_none_or(|newest| held > newest + interval);
			let wall = now.saturating_duration_since(*at);
			let media = min.newest.unwrap_or(held).saturating_sub(held);
			if max.exhausted() || leads || wall.max(media) >= interval {
				self.source = Lane::Min;
			}
		} else if max.exhausted() {
			self.source = Lane::Min;
		}

		let source = &mut self.lanes[self.source as usize];
		if let Some((frame, _)) = source.queue.pop_front() {
			if self.dual {
				self.last = Some(Duration::from(frame.timestamp));
			}
			return Next::Frame(frame);
		}

		if self.lanes.iter().all(State::exhausted) {
			return Next::End;
		}

		// Only a held min-delay picture has a deadline.
		let deadline = match self.source {
			Lane::Max => self.lanes[Lane::Min as usize]
				.queue
				.front()
				.map(|(_, at)| *at + interval),
			Lane::Min => None,
		};
		Next::Wait(deadline)
	}
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;

	const FRAME: Duration = Duration::from_millis(33);

	fn picture(millis: u64) -> Frame {
		let i420 = crate::I420::new(crate::Size::new(2, 2), vec![0x80; 6]).unwrap();
		Frame::new(crate::Surface::I420(i420), Timestamp::from_millis(millis).unwrap())
	}

	/// The millisecond timestamps of every picture `next` returns at `now`.
	fn drain(selector: &mut Selector, now: Instant) -> Vec<u128> {
		let mut out = Vec::new();
		while let Next::Frame(frame) = selector.next(now) {
			out.push(frame.timestamp.as_millis());
		}
		out
	}

	fn selector() -> Selector {
		Selector::new(true, Some(FRAME))
	}

	#[tokio::test]
	async fn min_delay_pictures_show_until_the_max_delay_decoder_catches_up() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();

		// A cold join: the min-delay decoder is at the live edge, the max-delay
		// one a sweep behind and racing through it.
		selector.push(Lane::Min, picture(1000), now);
		selector.push(Lane::Max, picture(0), now);
		assert_eq!(drain(&mut selector, now), [1000]);
		assert_eq!(selector.source, Lane::Min);

		selector.push(Lane::Max, picture(500), now);
		selector.push(Lane::Min, picture(1033), now);
		assert_eq!(drain(&mut selector, now), [1033]);

		// Caught up: the max-delay decoder reached the newest min-delay picture.
		selector.push(Lane::Max, picture(1033), now);
		selector.push(Lane::Max, picture(1066), now);
		assert_eq!(drain(&mut selector, now), [1066]);
		assert_eq!(selector.source, Lane::Max);
	}

	/// The max-delay decoder finishing its first picture before the min-delay
	/// one has any is not caught up: that picture is a sweep behind the live edge.
	#[tokio::test]
	async fn a_join_waits_for_the_min_delay_picture() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		assert!(matches!(selector.next(now), Next::Wait(None)));
		selector.push(Lane::Min, picture(1000), now);
		assert_eq!(drain(&mut selector, now), [1000]);
		assert_eq!(selector.source, Lane::Min);
	}

	#[tokio::test]
	async fn the_max_delay_decoder_keeps_the_picture_without_alternating() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);
		assert_eq!(selector.source, Lane::Max);

		// The two race on every frame; whichever decodes first, the max-delay
		// picture is the one shown.
		for index in 1..10u64 {
			selector.push(Lane::Min, picture(index * 33), now);
			assert!(matches!(selector.next(now), Next::Wait(Some(_))), "held, not shown");
			selector.push(Lane::Max, picture(index * 33), now);
			assert_eq!(drain(&mut selector, now), [u128::from(index * 33)]);
			assert_eq!(selector.source, Lane::Max, "alternated at frame {index}");
		}
	}

	#[tokio::test]
	async fn a_min_delay_skip_hands_the_picture_back_until_max_delay_catches_up() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);

		// The min-delay decoder skipped a stalled group; it leads by far more
		// than a frame, so it is shown at once.
		selector.push(Lane::Min, picture(1000), now);
		assert_eq!(drain(&mut selector, now), [1000]);
		assert_eq!(selector.source, Lane::Min);

		// The max-delay decoder waited out the stall and races through it,
		// never showing the pictures already passed.
		for millis in [33, 66, 500, 999] {
			selector.push(Lane::Max, picture(millis), now);
			assert_eq!(drain(&mut selector, now), Vec::<u128>::new());
		}
		selector.push(Lane::Min, picture(1033), now);
		assert_eq!(drain(&mut selector, now), [1033]);
		selector.push(Lane::Max, picture(1000), now);
		selector.push(Lane::Max, picture(1033), now);
		assert_eq!(drain(&mut selector, now), Vec::<u128>::new());
		assert_eq!(selector.source, Lane::Max, "caught up");
		selector.push(Lane::Max, picture(1066), now);
		assert_eq!(drain(&mut selector, now), [1066]);
	}

	#[tokio::test]
	async fn a_held_picture_shows_within_a_frame_of_wall_time() {
		tokio::time::pause();
		let start = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), start);
		selector.push(Lane::Min, picture(0), start);
		assert_eq!(drain(&mut selector, start), [0]);

		// One newer min-delay picture, then the publisher pauses: the max-delay
		// decoder never produces it.
		selector.push(Lane::Min, picture(33), start);
		let Next::Wait(Some(deadline)) = selector.next(start) else {
			panic!("a held picture must carry a deadline");
		};
		assert_eq!(deadline, start + FRAME);
		assert!(matches!(
			selector.next(deadline - Duration::from_millis(1)),
			Next::Wait(_)
		));
		assert_eq!(drain(&mut selector, deadline), [33]);
	}

	#[tokio::test]
	async fn a_held_picture_shows_within_a_frame_of_media_time() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);

		// No wall time passes (a caller draining faster than real time), but the
		// min-delay decoder moved a frame past the held picture.
		selector.push(Lane::Min, picture(20), now);
		assert!(matches!(selector.next(now), Next::Wait(_)));
		selector.push(Lane::Min, picture(53), now);
		assert_eq!(drain(&mut selector, now), [20, 53]);
	}

	#[tokio::test]
	async fn a_held_picture_shows_when_the_max_delay_decoder_stops() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);

		selector.push(Lane::Min, picture(33), now);
		assert!(matches!(selector.next(now), Next::Wait(_)));
		// The max-delay decoder ended, as at the end of the track.
		selector.finish(Lane::Max);
		assert_eq!(drain(&mut selector, now), [33]);
		assert_eq!(selector.source, Lane::Min);
	}

	#[tokio::test]
	async fn the_tail_drains_from_both_lanes() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);

		// The min-delay decoder ends first; the max-delay one still has a tail.
		selector.push(Lane::Min, picture(33), now);
		selector.finish(Lane::Min);
		selector.push(Lane::Max, picture(33), now);
		selector.push(Lane::Max, picture(66), now);
		assert_eq!(drain(&mut selector, now), [33, 66]);
		assert!(matches!(selector.next(now), Next::Wait(None)));
		selector.finish(Lane::Max);
		assert!(matches!(selector.next(now), Next::End));

		// And the other way around: max-delay ends first while min-delay drains.
		let mut selector = Selector::new(true, Some(FRAME));
		selector.push(Lane::Min, picture(0), now);
		selector.finish(Lane::Max);
		selector.push(Lane::Min, picture(33), now);
		selector.finish(Lane::Min);
		assert_eq!(drain(&mut selector, now), [0, 33]);
		assert!(matches!(selector.next(now), Next::End));

		// A lane that stopped still shows what it already decoded.
		let mut selector = Selector::new(true, Some(FRAME));
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);
		selector.finish(Lane::Min);
		selector.push(Lane::Max, picture(33), now);
		selector.finish(Lane::Max);
		assert_eq!(drain(&mut selector, now), [33]);
		assert!(matches!(selector.next(now), Next::End));
	}

	#[tokio::test]
	async fn one_lane_passes_every_picture_through() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = Selector::new(false, None);
		assert!(!selector.wants(Lane::Max));
		// Decode order, as a probe that does not reorder hands it back.
		for millis in [0, 66, 33] {
			selector.push(Lane::Min, picture(millis), now);
		}
		selector.finish(Lane::Min);
		assert_eq!(drain(&mut selector, now), [0, 66, 33]);
		assert!(matches!(selector.next(now), Next::End));
	}

	/// Whatever order the two lanes deliver in, and however often the source
	/// changes, what comes out strictly increases.
	#[tokio::test]
	async fn output_strictly_increases_across_every_switch() {
		tokio::time::pause();
		let start = Instant::now();
		let mut selector = Selector::new(true, None);
		// A deterministic interleaving with skips, stalls, and catch-ups on both.
		let min = [0, 33, 66, 1000, 1033, 1066, 1100, 2000, 2033, 2066, 2100, 2133];
		let max = [0, 33, 66, 100, 133, 1000, 1033, 1066, 1100, 1133, 2033, 2133, 2166];
		let mut out = Vec::new();
		let (mut a, mut b) = (0, 0);
		let mut now = start;
		while a < min.len() || b < max.len() {
			// Min-delay runs two steps for every max-delay step, then the reverse.
			let min_first = (a + b) % 5 < 3;
			if (min_first || b >= max.len()) && a < min.len() {
				selector.push(Lane::Min, picture(min[a]), now);
				a += 1;
			} else {
				selector.push(Lane::Max, picture(max[b]), now);
				b += 1;
			}
			now += Duration::from_millis(7);
			out.extend(drain(&mut selector, now));
		}
		selector.finish(Lane::Min);
		selector.finish(Lane::Max);
		out.extend(drain(&mut selector, now));
		assert!(out.windows(2).all(|pair| pair[0] < pair[1]), "not increasing: {out:?}");
		assert_eq!(out.last(), Some(&2166), "the newest picture was lost: {out:?}");
	}
}
