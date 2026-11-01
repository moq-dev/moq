//! Picks which of a min-delay and a max-delay decoder's pictures to show.
//!
//! An intra-refresh rendition has no keyframes, so a decoder that joins or skips
//! mid-sweep shows refresh stripes until a whole sweep has passed. The min-delay
//! decoder keeps the caller's latency and may show them; the max-delay decoder
//! tolerates one more sweep of delay, so it keeps continuity. Both race on the
//! same frames in steady state, and taking whichever is newest would flicker
//! stripes between clean pictures, so the source is sticky instead.
//!
//! Gaps and ends ride in each lane's queue between its pictures, so they come out
//! in order with whichever lane is the source. Both lanes read the same track, so
//! the first copy of an end returned stands for both, and a loss the output has
//! already moved past is no loss in it.
//!
//! [`Consumer`](super::Consumer) runs both decoders and this selector for you. A
//! player that paces its own decoding, such as `moq play`, drives a [`Selector`]
//! directly: it pushes each lane's output and returns whatever [`Selector::next`]
//! hands back.

use std::collections::VecDeque;
use std::time::Duration;

use kio::clock::Instant;

use super::Read;

/// One of the two decoders.
///
/// The discriminant indexes a two-element array, so a caller may keep its own
/// per-lane state as `[T; 2]` indexed by `lane as usize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
	/// Skips at the caller's latency target, and may show stripes after a join or a skip.
	Min = 0,
	/// Tolerates one more sweep, joining a group earlier, so its pictures stay clean.
	Max = 1,
}

/// Decoded pictures a lane may hold before its decoder waits for the selector.
///
/// Two, so a held min-delay picture still lets the decoder behind it move the
/// presentation clock the hold is measured on, without parking a backlog of
/// decoded surfaces a hardware pool may need back. Gaps and ends don't count:
/// they hold no surface, and a lane must reach its next picture past them to
/// lead the other.
const QUEUE: usize = 2;

/// What [`Selector::next`] decided.
pub enum Next {
	/// Show or apply this next.
	Read(Read),
	/// Nothing to show until a lane produces more, or until the deadline passes.
	Wait(Option<Instant>),
	/// Both lanes finished and nothing is left to show.
	Done,
}

#[derive(Default)]
struct State {
	queue: VecDeque<(Read, Instant)>,
	/// Pictures in the queue.
	pictures: usize,
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

	/// The first queued picture's timestamp, and when it was decoded.
	fn held(&self) -> Option<(Duration, Instant)> {
		self.queue.iter().find_map(|(read, at)| match read {
			Read::Frame(frame) => Some((Duration::from(frame.timestamp), *at)),
			_ => None,
		})
	}
}

/// Chooses between a min-delay and a max-delay decoder's pictures.
///
/// The max-delay picture is shown once that decoder has caught up to the
/// min-delay one, a newer min-delay picture waits at most one frame interval for
/// it, and with two lanes the returned picture timestamps strictly increase.
pub struct Selector {
	lanes: [State; 2],
	source: Lane,
	/// Whether there are two lanes to choose between.
	dual: bool,
	/// The last picture timestamp returned with two lanes, which output strictly
	/// increases past. A lone decoder's pictures pass through as they come.
	last: Option<Duration>,
	/// The furthest a gap returned with two lanes resumed at.
	resumed: Option<Duration>,
	/// The newest end returned with two lanes.
	ended: Option<Duration>,
	/// The catalog's frame interval, or the smallest step the min-delay lane has
	/// decoded. The smallest rather than the latest, so a skip never stretches it.
	interval: Option<Duration>,
	measured: bool,
}

impl Selector {
	/// A selector for one lane, or for both when `dual`.
	///
	/// `interval` is the catalog's frame interval, how long a newer min-delay
	/// picture waits for the max-delay decoder. Without one, the selector uses
	/// the smallest step the min-delay lane decodes.
	pub fn new(dual: bool, interval: Option<Duration>) -> Self {
		let mut lanes: [State; 2] = Default::default();
		lanes[Lane::Max as usize].done = !dual;
		Self {
			lanes,
			source: Lane::Min,
			dual,
			last: None,
			resumed: None,
			ended: None,
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
	pub fn wants(&self, lane: Lane) -> bool {
		let state = &self.lanes[lane as usize];
		!state.done && state.pictures < QUEUE
	}

	/// What `lane` read at `now`.
	///
	/// Between two pictures a lane keeps one gap, spanning every loss there, and its
	/// first end, where the pictures before it stop. Marker-only groups read while this
	/// lane waits behind the source then cost a bounded queue, not an entry each.
	pub fn push(&mut self, lane: Lane, read: Read, now: Instant) {
		let state = &mut self.lanes[lane as usize];
		let mut run = state
			.queue
			.iter_mut()
			.rev()
			.map(|(queued, _)| queued)
			.take_while(|queued| !matches!(queued, Read::Frame(_)));
		match &read {
			Read::Frame(frame) => {
				let timestamp = Duration::from(frame.timestamp);
				if lane == Lane::Min
					&& self.measured
					&& let Some(step) = state.newest.and_then(|newest| timestamp.checked_sub(newest))
					&& !step.is_zero()
				{
					self.interval = Some(self.interval.map_or(step, |interval| interval.min(step)));
				}
				state.newest = Some(state.newest.map_or(timestamp, |newest| newest.max(timestamp)));
				state.pictures += 1;
			}
			Read::Gap { timestamp, duration } => {
				if let Some(Read::Gap {
					timestamp: start,
					duration: span,
				}) = run.find(|queued| matches!(queued, Read::Gap { .. }))
				{
					(*start, *span) = merge((*start, *span), (*timestamp, *duration));
					return;
				}
			}
			Read::End { .. } => {
				if run.any(|queued| matches!(queued, Read::End { .. })) {
					return;
				}
			}
		}
		state.queue.push_back((read, now));
	}

	/// Forget every queued picture and what each lane has decoded, as after a
	/// break that discards both decoders' output. What was already returned is
	/// kept, so the output still never rewinds.
	pub fn clear(&mut self) {
		for state in &mut self.lanes {
			state.queue.clear();
			state.pictures = 0;
			state.newest = None;
		}
	}

	/// `lane` ended and will decode nothing more.
	pub fn finish(&mut self, lane: Lane) {
		self.lanes[lane as usize].done = true;
	}

	/// The next item to return at `now`, or what to wait for.
	pub fn next(&mut self, now: Instant) -> Next {
		if self.dual {
			self.prune();
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
		} else if let Some((held, at)) = min.held() {
			// A newer min-delay picture waits for the max-delay decoder to reach
			// it, but never for longer than a frame: not when it already leads by
			// more than that, not once a frame's worth of wall or media time has
			// passed, and not after the max-delay decoder has nothing left.
			let leads = max.newest.is_none_or(|newest| held > newest + interval);
			let wall = now.saturating_duration_since(at);
			let media = min.newest.unwrap_or(held).saturating_sub(held);
			if max.exhausted() || leads || wall.max(media) >= interval {
				self.source = Lane::Min;
			}
		} else if max.exhausted() {
			self.source = Lane::Min;
		}

		let source = &mut self.lanes[self.source as usize];
		if let Some((read, _)) = source.queue.pop_front() {
			match &read {
				Read::Frame(frame) => {
					source.pictures -= 1;
					if self.dual {
						self.last = Some(Duration::from(frame.timestamp));
					}
				}
				Read::Gap { timestamp, duration } if self.dual => {
					self.resumed = Some(Duration::from(*timestamp) + Duration::from(*duration));
				}
				Read::End { end } if self.dual => self.ended = Some(Duration::from(*end)),
				_ => {}
			}
			return Next::Read(read);
		}

		if self.lanes.iter().all(State::exhausted) {
			return Next::Done;
		}

		// Only a held min-delay picture has a deadline.
		let deadline = match self.source {
			Lane::Max => self.lanes[Lane::Min as usize].held().map(|(_, at)| at + interval),
			Lane::Min => None,
		};
		Next::Wait(deadline)
	}

	/// Drop what the output already moved past, so a cutover never repeats or
	/// rewinds a picture, and never returns an end or a loss twice.
	fn prune(&mut self) {
		let (last, resumed, ended) = (self.last, self.resumed, self.ended);
		for state in &mut self.lanes {
			state.queue.retain(|(read, _)| match read {
				Read::Frame(frame) => last.is_none_or(|last| Duration::from(frame.timestamp) > last),
				// Lost by this lane, but the output already reached where it resumes.
				Read::Gap { timestamp, duration } => {
					let resume = Duration::from(*timestamp) + Duration::from(*duration);
					last.max(resumed).is_none_or(|reached| resume > reached)
				}
				// Returned from the other lane, or stale behind pictures shown past it.
				Read::End { end } => {
					let end = Duration::from(*end);
					ended.is_none_or(|ended| end > ended) && last.is_none_or(|last| end >= last)
				}
			});
			state.pictures = state
				.queue
				.iter()
				.filter(|(read, _)| matches!(read, Read::Frame(_)))
				.count();
		}
	}
}

/// One gap covering two with no picture between them, or the newer alone if their
/// scales differ.
fn merge(
	first: (moq_net::Timestamp, moq_net::Timestamp),
	second: (moq_net::Timestamp, moq_net::Timestamp),
) -> (moq_net::Timestamp, moq_net::Timestamp) {
	let merged = || {
		let start = first.0.min(second.0);
		let earlier = first.0.checked_add(first.1).ok()?;
		let later = second.0.checked_add(second.1).ok()?;
		Some((start, earlier.max(later).checked_sub(start).ok()?))
	};
	merged().unwrap_or(second)
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;

	const FRAME: Duration = Duration::from_millis(33);

	fn picture(millis: u64) -> Read {
		let i420 = crate::I420::new(crate::Size::new(2, 2), vec![0x80; 6]).unwrap();
		Read::Frame(crate::Frame::new(
			crate::Surface::I420(i420),
			Timestamp::from_millis(millis).unwrap(),
		))
	}

	fn gap(millis: u64, duration: u64) -> Read {
		Read::Gap {
			timestamp: Timestamp::from_millis(millis).unwrap(),
			duration: Timestamp::from_millis(duration).unwrap(),
		}
	}

	fn end(millis: u64) -> Read {
		Read::End {
			end: Timestamp::from_millis(millis).unwrap(),
		}
	}

	/// Everything `next` returns at `now`, as `frame 33`, `gap 66+100`, or `end 99`.
	fn reads(selector: &mut Selector, now: Instant) -> Vec<String> {
		let mut out = Vec::new();
		while let Next::Read(read) = selector.next(now) {
			out.push(match read {
				Read::Frame(frame) => format!("frame {}", frame.timestamp.as_millis()),
				Read::Gap { timestamp, duration } => format!("gap {}+{}", timestamp.as_millis(), duration.as_millis()),
				Read::End { end } => format!("end {}", end.as_millis()),
			});
		}
		out
	}

	/// The millisecond timestamps of every picture `next` returns at `now`.
	fn drain(selector: &mut Selector, now: Instant) -> Vec<u128> {
		let mut out = Vec::new();
		while let Next::Read(read) = selector.next(now) {
			let Read::Frame(frame) = read else {
				panic!("expected only pictures");
			};
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
		assert!(matches!(selector.next(now), Next::Done));

		// And the other way around: max-delay ends first while min-delay drains.
		let mut selector = Selector::new(true, Some(FRAME));
		selector.push(Lane::Min, picture(0), now);
		selector.finish(Lane::Max);
		selector.push(Lane::Min, picture(33), now);
		selector.finish(Lane::Min);
		assert_eq!(drain(&mut selector, now), [0, 33]);
		assert!(matches!(selector.next(now), Next::Done));

		// A lane that stopped still shows what it already decoded.
		let mut selector = Selector::new(true, Some(FRAME));
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(drain(&mut selector, now), [0]);
		selector.finish(Lane::Min);
		selector.push(Lane::Max, picture(33), now);
		selector.finish(Lane::Max);
		assert_eq!(drain(&mut selector, now), [33]);
		assert!(matches!(selector.next(now), Next::Done));
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
		assert!(matches!(selector.next(now), Next::Done));
	}

	#[tokio::test]
	async fn one_lane_passes_gaps_and_ends_through_in_order() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = Selector::new(false, None);
		for read in [picture(0), gap(33, 967), picture(1000), end(1033), picture(2000)] {
			selector.push(Lane::Min, read, now);
		}
		selector.finish(Lane::Min);
		assert_eq!(
			reads(&mut selector, now),
			["frame 0", "gap 33+967", "frame 1000", "end 1033", "frame 2000"]
		);
		assert!(matches!(selector.next(now), Next::Done));
	}

	/// Both decoders read the same end; it comes out once, between the stopped
	/// encoder's pictures and the fresh one's.
	#[tokio::test]
	async fn an_end_both_lanes_read_is_returned_once() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(reads(&mut selector, now), ["frame 0"]);

		// The min-delay decoder reaches the end first, while the source is max-delay.
		selector.push(Lane::Min, picture(33), now);
		selector.push(Lane::Min, end(66), now);
		for read in [picture(33), end(66), picture(1000)] {
			selector.push(Lane::Max, read, now);
		}
		assert_eq!(reads(&mut selector, now), ["frame 33", "end 66", "frame 1000"]);
		selector.push(Lane::Min, picture(1000), now);
		selector.push(Lane::Min, picture(1033), now);
		selector.push(Lane::Max, picture(1033), now);
		assert_eq!(reads(&mut selector, now), ["frame 1033"]);
	}

	/// A loss on one decoder that the other delivered past is no loss in what
	/// comes out; one on the decoder that takes over is.
	#[tokio::test]
	async fn a_gap_counts_only_when_the_output_skips_it() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(reads(&mut selector, now), ["frame 0"]);

		// The min-delay decoder lost 33..66, which the max-delay one decoded.
		selector.push(Lane::Max, picture(33), now);
		selector.push(Lane::Max, picture(66), now);
		selector.push(Lane::Min, gap(33, 33), now);
		selector.push(Lane::Min, picture(66), now);
		assert_eq!(reads(&mut selector, now), ["frame 33", "frame 66"]);

		// Then it skips a stalled group the max-delay decoder is still waiting on.
		selector.push(Lane::Min, gap(99, 901), now);
		selector.push(Lane::Min, picture(1000), now);
		assert_eq!(reads(&mut selector, now), ["gap 99+901", "frame 1000"]);
		assert_eq!(selector.source, Lane::Min);
	}

	/// Boundaries the min-delay decoder reads while the max-delay source stalls fold
	/// into one gap and the first end, however many marker groups arrive.
	#[tokio::test]
	async fn a_run_of_boundaries_holds_one_gap_and_one_end() {
		tokio::time::pause();
		let now = Instant::now();
		let mut selector = selector();
		selector.push(Lane::Max, picture(0), now);
		selector.push(Lane::Min, picture(0), now);
		assert_eq!(reads(&mut selector, now), ["frame 0"]);

		selector.push(Lane::Min, end(33), now);
		for index in 1..1000 {
			selector.push(Lane::Min, gap(33 * index, 10), now);
			selector.push(Lane::Min, end(33 * index + 10), now);
		}
		assert_eq!(selector.lanes[Lane::Min as usize].queue.len(), 2);
		selector.push(Lane::Min, picture(40_000), now);
		assert_eq!(reads(&mut selector, now), ["end 33", "gap 33+32944", "frame 40000"]);
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
