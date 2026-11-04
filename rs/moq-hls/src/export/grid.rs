//! Numbering a rendition's groups into HLS segments on the broadcast's join-interval grid.
//!
//! HLS numbers segments by position, so every edge and every reload has to derive the same
//! consecutive numbers from timestamps alone. The catalog's `media.join.interval` fixes the
//! segment length, the smallest whole multiple of the interval that is at least [`MIN_SEGMENT`]
//! (see [`segment_length`]), which draws a grid line at every multiple of that length in pts.
//! Every rendition numbers its segments on it, so they share segment numbers while each cuts its
//! boundaries at its own groups.
//!
//! A group spans from its first frame's pts to the next group's. It belongs to one segment: the
//! first line it contains, so segment N starts at the group containing pts N × length; or, if it
//! contains none, the line it starts after, `floor(pts / length)`. Groups are never split, since a
//! segment starting mid-GOP is not independent. A grid slot no group starts a segment in, because
//! an overrun or a pause skipped it, is listed as a gap, so the numbering never skips: on a 2s grid,
//! a group A = [0s, 7s) is segment 0, slots 1 and 2 are gaps, and a contiguous B = [7s, 7.2s)
//! starts segment 3. An edge joining at B derives that from B's and its successor's timestamps,
//! without A.
//!
//! A segment starts at its first group's pts, or at its line when that group starts after it, as
//! a group resuming from a pause does, and a gap at its line; its `EXTINF` runs to the next
//! segment's start. Every segment then starts after the line before its own and ends at or before
//! the next, so every `EXTINF` fits twice the segment length, the target duration. A group overrunning the segment length is listed at its first line, so its
//! `EXTINF` understates its media, and the gaps after it overstate theirs; the playlist timeline
//! still meets the media again at the next segment.

use std::time::Duration;

use moq_net::Timestamp;

/// The shortest segment an export cuts (maintainer, 2026-11-03): a join interval shorter than this,
/// as an audio-only broadcast's group spacing is, packs whole intervals into each segment rather
/// than listing a segment per group.
pub(crate) const MIN_SEGMENT: Duration = Duration::from_secs(1);

/// The segment length on a join interval of `interval`: the smallest whole multiple of it that is
/// at least [`MIN_SEGMENT`]. It derives from the interval alone, so every edge agrees, and a line
/// every segment starts at is also a line of the interval's own grid.
pub(crate) fn segment_length(interval: Duration) -> Duration {
	let interval = interval.as_nanos().max(1);
	let length = MIN_SEGMENT.as_nanos().div_ceil(interval).max(1) * interval;
	Duration::from_nanos(u64::try_from(length).unwrap_or(u64::MAX))
}

/// One segment the grid completed, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
	/// A segment holding groups.
	Media {
		/// The segment's number on the grid.
		segment: u64,
		/// Its groups, in sequence order: from its first group up to the next segment's.
		groups: std::ops::Range<u64>,
		/// Its first group's pts.
		pts: Timestamp,
		/// Where its `EXTINF` starts: its first group's pts, or its line when that group starts
		/// after it, as a group resuming from a pause in the slot it lands in does. The time from the
		/// line to the group heads the segment, as a gap's does, rather than trailing the segment
		/// before it.
		start: Timestamp,
		/// Where its `EXTINF` ends: the next segment's start.
		end: Timestamp,
		/// Where its last group's content ends, the next group's pts, when known: a fallback for
		/// timing its last sample.
		content: Option<Timestamp>,
	},
	/// A grid slot no group starts a segment in (`EXT-X-GAP`).
	Gap {
		/// The segment's number on the grid.
		segment: u64,
		/// Its line.
		pts: Timestamp,
		/// The next segment's start.
		end: Timestamp,
	},
	/// A group started before the one before it, as a publisher restarting its timeline on the
	/// same track does: what came before can't be numbered or muxed beside it.
	Reset,
}

/// A group seen but not yet placed: placing it needs its end, the next group's pts.
#[derive(Clone, Copy, Debug)]
struct Group {
	sequence: u64,
	pts: Timestamp,
}

/// The segment collecting groups, completed once a group lands in a later one.
#[derive(Clone, Debug)]
struct Open {
	segment: u64,
	groups: std::ops::Range<u64>,
	pts: Timestamp,
	/// Where a discontinuity marker after its last group stopped the content, if one did.
	stop: Option<Timestamp>,
}

impl Open {
	fn new(segment: u64, group: Group) -> Self {
		Self {
			segment,
			groups: group.sequence..group.sequence + 1,
			pts: group.pts,
			stop: None,
		}
	}
}

/// Places one rendition's groups, fed in sequence order, into segments on a grid of one length.
pub(crate) struct Grid {
	/// The segment length in nanoseconds, at least one.
	length: u128,
	/// The newest group, until the next one bounds it.
	pending: Option<Group>,
	open: Option<Open>,
	/// The newest group's pts, which the next must not precede.
	last: Option<Timestamp>,
	/// The most gaps listed for one skip: older ones would slide out of every playlist window.
	max_gaps: u64,
}

impl Slot {
	/// The segment's number, or `None` for a [`Reset`](Self::Reset).
	pub fn segment(&self) -> Option<u64> {
		match self {
			Self::Media { segment, .. } | Self::Gap { segment, .. } => Some(*segment),
			Self::Reset => None,
		}
	}
}

impl Grid {
	/// A grid of segments `length` long, listing at most `max_gaps` gaps for one skip.
	pub fn new(length: Duration, max_gaps: usize) -> Self {
		Self {
			length: length.as_nanos().max(1),
			pending: None,
			open: None,
			last: None,
			max_gaps: max_gaps as u64,
		}
	}

	/// The segment a group starting at `pts` and ending at `end` belongs to, or `None` while it
	/// needs an `end` it doesn't have.
	pub fn segment(&self, pts: Timestamp, end: Option<Timestamp>) -> Option<u64> {
		let pts = pts.as_nanos();
		let slot = pts / self.length;
		if pts.is_multiple_of(self.length) {
			return Some(slot as u64);
		}
		let line = (slot + 1) * self.length;
		let end = end?.as_nanos();
		Some(if end > line { slot + 1 } else { slot } as u64)
	}

	/// Whether `slot` is known whole to a reader that saw nothing before it: a gap, or a segment
	/// whose first group contains its line, since the segment starts at that group. One whose first
	/// group starts after its line may follow earlier groups of the same segment.
	pub fn starts(&self, slot: &Slot) -> bool {
		match slot {
			Slot::Media { segment, pts, .. } => (*segment as u128).saturating_mul(self.length) >= pts.as_nanos(),
			Slot::Gap { .. } => true,
			Slot::Reset => false,
		}
	}

	/// The open segment, which a later group or the track's end completes.
	pub fn open(&self) -> Option<u64> {
		self.open.as_ref().map(|open| open.segment)
	}

	/// The groups of the open segment so far.
	pub fn open_groups(&self) -> Option<std::ops::Range<u64>> {
		self.open.as_ref().map(|open| open.groups.clone())
	}

	/// Whether a segment waits to be completed: one is open, or a group waits for its end.
	pub fn waiting(&self) -> bool {
		self.open.is_some() || self.pending.is_some()
	}

	/// Where segment `segment`, whose first group starts at `pts`, starts its `EXTINF`: that group,
	/// or its line when the group starts after it.
	///
	/// Every `EXTINF` then fits twice the segment length, the target duration: a segment's first
	/// group contains its line or starts after it, so the segment starts after the line before its
	/// own, and it ends where the next starts, at or before the next line. A gap runs from its line
	/// to the next start, at most one length. Only a final segment whose content runs past the next
	/// line can exceed it.
	fn start(&self, segment: u64, pts: Timestamp) -> Timestamp {
		let line = self.line(segment);
		if pts.as_nanos() > line.as_nanos() { line } else { pts }
	}

	/// The pts of line `segment`.
	fn line(&self, segment: u64) -> Timestamp {
		let nanos = (segment as u128).saturating_mul(self.length);
		Timestamp::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)).unwrap_or(Timestamp::ZERO)
	}

	/// A group `sequence` starts at `pts`, after every group already fed. Returns the segments it
	/// completes.
	pub fn group(&mut self, sequence: u64, pts: Timestamp) -> Vec<Slot> {
		let mut out = Vec::new();
		// A group starting before the one before it, even within one slot, can't share a segment
		// or a numbering with what came before, since its media would run backwards.
		if self.last.replace(pts).is_some_and(|last| pts < last) {
			self.pending = None;
			self.open = None;
			out.push(Slot::Reset);
		}
		if let Some(pending) = self.pending.take() {
			let segment = self.segment(pending.pts, Some(pts)).expect("bounded");
			self.place(pending, segment, &mut out);
		}
		let group = Group { sequence, pts };
		// A group starting on a line needs no end to place, so the segment before it completes now.
		match self.segment(pts, None) {
			Some(segment) => self.place(group, segment, &mut out),
			None => self.pending = Some(group),
		}
		out
	}

	/// A discontinuity marker at `at` stopped the content: the newest group ends there rather than
	/// where the next group starts. Returns the segments it completes.
	pub fn stop(&mut self, at: Timestamp) -> Vec<Slot> {
		let mut out = Vec::new();
		if let Some(pending) = self.pending.take() {
			let segment = self.segment(pending.pts, Some(at)).expect("bounded");
			self.place(pending, segment, &mut out);
		}
		if let Some(open) = self.open.as_mut() {
			open.stop = Some(at);
		}
		out
	}

	/// The track ended, its last group's last frame starting at `reach` and its content ending at
	/// `end`, when known, unless a marker stopped it first. Returns the segments left.
	pub fn finish(&mut self, reach: Option<Timestamp>, end: Option<Timestamp>) -> Vec<Slot> {
		let mut out = Vec::new();
		if let Some(pending) = self.pending.take() {
			// The last group spans to its last frame: without one, it reaches no further line.
			let segment = self
				.segment(pending.pts, Some(reach.unwrap_or(pending.pts)))
				.expect("bounded");
			self.place(pending, segment, &mut out);
		}
		if let Some(open) = self.open.take() {
			let end = [open.stop, end, reach]
				.into_iter()
				.flatten()
				.find(|end| end.as_nanos() > open.pts.as_nanos())
				.unwrap_or(open.pts);
			out.push(Slot::Media {
				segment: open.segment,
				groups: open.groups,
				pts: open.pts,
				start: self.start(open.segment, open.pts),
				end,
				content: Some(end),
			});
		}
		out
	}

	/// Place `group` in `segment`, completing the open segment and the gaps before it when it is a
	/// later one.
	fn place(&mut self, group: Group, segment: u64, out: &mut Vec<Slot>) {
		let Some(open) = self.open.as_mut() else {
			self.open = Some(Open::new(segment, group));
			return;
		};
		if segment == open.segment {
			open.groups.end = group.sequence + 1;
			// The content resumed within the segment, so it runs to whatever ends this group.
			open.stop = None;
			return;
		}
		if segment < open.segment {
			out.push(Slot::Reset);
			self.open = Some(Open::new(segment, group));
			return;
		}
		let open = self.open.take().expect("checked above");
		let start = |next: u64| match next == segment {
			true => self.start(segment, group.pts),
			false => self.line(next),
		};
		out.push(Slot::Media {
			segment: open.segment,
			groups: open.groups,
			pts: open.pts,
			start: self.start(open.segment, open.pts),
			end: start(open.segment + 1),
			content: Some(open.stop.unwrap_or(group.pts)),
		});
		let first = (open.segment + 1).max(segment.saturating_sub(self.max_gaps));
		for gap in first..segment {
			out.push(Slot::Gap {
				segment: gap,
				pts: self.line(gap),
				end: start(gap + 1),
			});
		}
		self.open = Some(Open::new(segment, group));
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn ms(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
	}

	/// A slot as `(segment, groups, EXTINF start ms, end ms)`, with no groups for a gap.
	type Listed = (u64, std::ops::Range<u64>, u64, u64);

	fn listed(slots: Vec<Slot>) -> Vec<Listed> {
		slots
			.into_iter()
			.map(|slot| match slot {
				Slot::Media {
					segment,
					groups,
					start,
					end,
					..
				} => (segment, groups, start.as_millis() as u64, end.as_millis() as u64),
				Slot::Gap { segment, pts, end } => (segment, 0..0, pts.as_millis() as u64, end.as_millis() as u64),
				Slot::Reset => panic!("unexpected reset"),
			})
			.collect()
	}

	/// Feed groups starting at `starts` ms, numbered from zero, then end the track at `end` ms.
	fn run(length_ms: u64, starts: &[u64], end: u64) -> Vec<Listed> {
		let mut grid = Grid::new(Duration::from_millis(length_ms), 256);
		let mut slots = Vec::new();
		for (sequence, start) in starts.iter().enumerate() {
			slots.extend(grid.group(sequence as u64, ms(*start)));
		}
		slots.extend(grid.finish(Some(ms(end)), Some(ms(end))));
		listed(slots)
	}

	/// Each case: segment length, group starts, content end, and the segments listed.
	#[test]
	fn segmentation() {
		#[allow(clippy::type_complexity, reason = "a table of plain tuples reads best inline")]
		let cases: &[(&str, u64, &[u64], u64, &[Listed])] = &[
			(
				"regular GOPs on the grid",
				2000,
				&[0, 2000, 4000],
				6000,
				&[(0, 0..1, 0, 2000), (1, 1..2, 2000, 4000), (2, 2..3, 4000, 6000)],
			),
			(
				"GOPs off the grid start at the group containing each line",
				2000,
				&[500, 1900, 3800, 5700],
				7600,
				&[
					(0, 0..1, 0, 1900),
					(1, 1..2, 1900, 3800),
					(2, 2..3, 3800, 5700),
					(3, 3..4, 5700, 7600),
				],
			),
			(
				"short groups pack into their slot",
				2000,
				&[0, 500, 1000, 1500, 2000, 2500],
				3000,
				&[(0, 0..4, 0, 2000), (1, 4..6, 2000, 3000)],
			),
			(
				"unequal GOPs, one containing no line joining the one before",
				2000,
				&[0, 1000, 2600, 3200, 6000],
				8000,
				&[
					(0, 0..1, 0, 1000),
					(1, 1..3, 1000, 3200),
					(2, 3..4, 3200, 6000),
					(3, 4..5, 6000, 8000),
				],
			),
			(
				"an overrun lists its first line and the slots it covers as gaps",
				2000,
				&[0, 7000, 7200],
				8000,
				&[
					(0, 0..1, 0, 2000),
					(1, 0..0, 2000, 4000),
					(2, 0..0, 4000, 6000),
					(3, 1..3, 6000, 8000),
				],
			),
			(
				"an overrun followed by a group starting on a line",
				2000,
				&[0, 6000],
				8000,
				&[
					(0, 0..1, 0, 2000),
					(1, 0..0, 2000, 4000),
					(2, 0..0, 4000, 6000),
					(3, 1..2, 6000, 8000),
				],
			),
			(
				"a pause ending between lines",
				2000,
				&[0, 500, 7300],
				8000,
				&[
					(0, 0..1, 0, 500),
					(1, 1..2, 500, 4000),
					(2, 0..0, 4000, 6000),
					(3, 2..3, 6000, 8000),
				],
			),
			(
				"a pause resuming past its slot's line times that segment from the line",
				2000,
				&[10, 5990],
				5995,
				&[(1, 0..1, 10, 4000), (2, 1..2, 4000, 5995)],
			),
			(
				"a pause ending past the next line",
				2000,
				&[0, 2000, 9000],
				10000,
				&[
					(0, 0..1, 0, 2000),
					(1, 1..2, 2000, 4000),
					(2, 0..0, 4000, 6000),
					(3, 0..0, 6000, 8000),
					(4, 2..3, 8000, 10000),
				],
			),
		];
		for (name, interval, starts, end, expected) in cases {
			assert_eq!(run(*interval, starts, *end), *expected, "{name}");
		}
	}

	/// Segments are whole intervals at least a second long: 20 ms audio groups pack into 1 s
	/// segments, an odd 300 ms interval into 1.2 s ones, and a 2 s GOP stays 2 s.
	#[test]
	fn segments_are_whole_intervals_of_at_least_a_second() {
		for (interval, length) in [
			(20, 1000),
			(300, 1200),
			(999, 1998),
			(1000, 1000),
			(2000, 2000),
			(2500, 2500),
		] {
			assert_eq!(
				segment_length(Duration::from_millis(interval)),
				Duration::from_millis(length),
				"{interval} ms"
			);
		}
		assert_eq!(
			segment_length(Duration::from_nanos(333_333_333)),
			Duration::from_nanos(999_999_999 + 333_333_333)
		);
		assert_eq!(segment_length(Duration::ZERO), MIN_SEGMENT);
	}

	/// Every `EXTINF` fits the target duration, twice the segment length (RFC 8216 4.3.3.1), across
	/// GOPs within the interval, overruns, pauses, and groups resuming just past a line, for edges
	/// joining at any group. Only a final segment can reach past two lengths, by its last frame.
	#[test]
	fn every_extinf_fits_the_target() {
		// A small LCG, so the cases are the same on every run.
		let mut seed: u64 = 0x5eed;
		let mut next = move |below: u64| {
			seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
			(seed >> 33) % below.max(1)
		};
		for interval in [20u64, 300, 999, 2_000, 3_000] {
			let length = segment_length(Duration::from_millis(interval)).as_millis() as u64;
			let target = (2 * length).div_ceil(1_000);
			for _ in 0..50 {
				let mut starts = vec![next(length)];
				for _ in 0..60 {
					let last = *starts.last().unwrap();
					let step = match next(10) {
						// A GOP within the interval.
						0..=5 => 1 + next(interval),
						// An overrun: a group longer than the interval.
						6 => interval + next(3 * length),
						// A pause.
						7 => length + next(5 * length),
						// A group resuming just past a line, or on it.
						_ => (last / length + 1 + next(3)) * length + next(4) - last,
					};
					starts.push(last + step.max(1));
				}
				let end = starts.last().unwrap() + 1;
				for join in [0, 1, starts.len() / 2] {
					let mut grid = Grid::new(Duration::from_millis(length), 256);
					let mut slots = Vec::new();
					for (sequence, start) in starts.iter().enumerate().skip(join) {
						slots.extend(grid.group(sequence as u64, ms(*start)));
					}
					slots.extend(grid.finish(Some(ms(end - 1)), Some(ms(end))));
					let listed = listed(slots);
					for (index, (segment, _, start, end)) in listed.iter().enumerate() {
						let extinf = end - start;
						let fits = ((extinf as f64) / 1_000.0).round() as u64 <= target;
						assert!(fits, "{interval} ms: segment {segment} lasts {extinf} ms: {starts:?}");
						if index + 1 < listed.len() {
							assert!(
								extinf < 2 * length,
								"{interval} ms: segment {segment} lasts {extinf} ms: {starts:?}"
							);
						}
					}
				}
			}
		}
	}

	/// The maintainer's 2026-11-03 rule: an edge joining at B numbers it 3, as one that saw A does.
	#[test]
	fn a_late_join_numbers_like_an_early_one() {
		let early = run(2000, &[0, 7000, 7200], 7400);
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		let mut late = grid.group(1, ms(7000));
		late.extend(grid.group(2, ms(7200)));
		late.extend(grid.finish(Some(ms(7400)), Some(ms(7400))));
		assert_eq!(listed(late), [(3, 1..3, 6000, 7400)]);
		assert_eq!(early.last(), Some(&(3, 1..3, 6000, 7400)));
	}

	/// A group off the grid waits for its successor; one on a line is placed at once.
	#[test]
	fn a_segment_completes_once_a_later_one_starts() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(0)).is_empty());
		assert!(
			grid.group(1, ms(1000)).is_empty(),
			"its successor may still reach the next line"
		);
		assert_eq!(listed(grid.group(2, ms(2000))), [(0, 0..2, 0, 2000)]);
		assert!(grid.group(3, ms(3500)).is_empty());
		assert_eq!(
			listed(grid.group(4, ms(4500))),
			[(1, 2..3, 2000, 3500)],
			"3.5s reaches 4s"
		);
	}

	/// A long pause lists only the newest gaps, the ones a playlist window can still hold.
	#[test]
	fn a_long_pause_lists_at_most_the_newest_gaps() {
		let mut grid = Grid::new(Duration::from_secs(2), 3);
		grid.group(0, ms(0));
		let slots = listed(grid.group(1, ms(3_600_000)));
		assert_eq!(slots.len(), 4);
		assert_eq!(slots[0], (0, 0..1, 0, 2000));
		assert_eq!(slots[1].0, 1797);
		assert_eq!(slots[3], (1799, 0..0, 3_598_000, 3_600_000));
	}

	/// A group going back within the open slot resets too: its media can't follow what it precedes.
	#[test]
	fn a_rewind_within_a_slot_resets() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(10_500)).is_empty());
		assert_eq!(grid.group(1, ms(10_200)), [Slot::Reset]);
		assert_eq!(
			listed(grid.finish(Some(ms(10_400)), Some(ms(10_400)))),
			[(5, 1..2, 10_000, 10_400)],
			"only what follows the rewind is listed"
		);
	}

	/// A marker ends the content before it, so the last group's samples stop there rather than
	/// running through the pause to the group that resumes.
	#[test]
	fn a_marker_stops_the_content() {
		let content = |slots: Vec<Slot>| -> Vec<Option<u64>> {
			slots
				.into_iter()
				.filter_map(|slot| match slot {
					Slot::Media { content, .. } => Some(content.map(|at| at.as_millis() as u64)),
					_ => None,
				})
				.collect()
		};
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0));
		grid.group(1, ms(1000));
		assert!(content(grid.stop(ms(1500))).is_empty(), "the slot is still open");
		assert_eq!(content(grid.group(2, ms(8000))), [Some(1500)]);

		// The track ending after a marker ends at the marker.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0));
		grid.stop(ms(700));
		assert_eq!(listed(grid.finish(None, None)), [(0, 0..1, 0, 700)]);

		// Content resuming within the slot runs on to whatever ends it.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0));
		grid.stop(ms(300));
		grid.group(1, ms(1000));
		assert_eq!(content(grid.group(2, ms(2000))), [Some(2000)]);
	}

	/// A group going back past the open slot resets the numbering.
	#[test]
	fn a_rewind_resets() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(10_000));
		assert_eq!(grid.group(1, ms(0)), [Slot::Reset]);
		assert_eq!(
			listed(grid.finish(Some(ms(2000)), Some(ms(2000)))),
			[(0, 1..2, 0, 2000)]
		);
	}
}
