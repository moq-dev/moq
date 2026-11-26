//! Numbering a rendition's groups into HLS segments on the broadcast's join-interval grid.
//!
//! HLS numbers segments by position, so every edge and every reload has to derive the same
//! consecutive numbers from timestamps alone. The catalog's `sync.interval` fixes the
//! segment length, the smallest whole multiple of the interval that is at least [`MIN_SEGMENT`]
//! (see [`segment_length`]), which draws a grid line at every multiple of that length in pts.
//! Every rendition numbers its segments on it, so they share segment numbers while each cuts its
//! boundaries at its own groups.
//!
//! A group starts at its key on a timed track, or its first frame's pts where the session carries
//! no timeline, and ends where the next group starts. It belongs to one segment. A group starting on a line, or at most a sixteenth of a segment after one (the
//! slack), belongs to that line's segment, so a keyframe landing a little late, as timestamp jitter
//! or an NTSC GOP of 2,002 ms on a 2 s grid puts it, still starts that segment rather than leaving
//! it a gap of a few milliseconds. Any other group belongs to the next line if it contains it, so
//! segment N starts at the group containing pts N × length, and otherwise to the line it starts
//! after, `floor(start / length)`. A group off the grid is placed once its end is known, or sooner
//! once one of its own frames passes the next line, since it then contains that line: every frame
//! of its successor presents after all of its own. The segment before it completes then, not a
//! group later. A segment also completes once its newest group's GROUP_END reaches the next line,
//! since every later group then starts a later segment. A GROUP_END places no group: one arriving
//! before or after the successor would then place it differently on different edges. Groups are never split, since a segment
//! starting mid-GOP is not independent. A grid slot no group starts a segment in, because an
//! overrun or a pause skipped it, is listed as a gap, so the numbering never skips: on a 2s grid, a
//! group A = [0.5s, 7s) is segment 0, slots 1 and 2 are gaps, and a contiguous B = [7s, 7.2s)
//! starts segment 3. An edge joining at B derives that from B's timestamps, without A.
//!
//! A segment starts at its first group's start, or at its line when that group starts after it, as
//! a group resuming from a pause does, and a gap at its line; its `EXTINF` runs to the next
//! segment's start. Every segment then starts after the line before its own and ends at or before
//! the next, so every `EXTINF` fits twice the segment length, the target duration. A group
//! overrunning the segment length is listed at its first line, so its `EXTINF` understates its
//! media, and the gaps after it overstate theirs; the playlist timeline still meets the media again
//! at the next segment.

use std::time::Duration;

use moq_net::Timestamp;

/// The shortest segment an export cuts (maintainer, 2026-11-03): a join interval shorter than this,
/// as an audio-only broadcast's group spacing is, packs whole intervals into each segment rather
/// than listing a segment per group.
pub(crate) const MIN_SEGMENT: Duration = Duration::from_secs(1);

/// The rules placing groups into segments, bumped whenever a group may land in another segment
/// than before, so a segment URL never names different groups on edges running different rules.
pub(crate) const RULES: u32 = 2;

/// How far past a line, as a fraction of the segment length, a group may start and still start that
/// line's segment: a sixteenth, at least a frame at 24 fps on the shortest segment.
const SLACK: u128 = 16;

/// The segment length on a join interval of `interval`: the smallest whole multiple of it that is
/// at least [`MIN_SEGMENT`]. It derives from the interval alone, so every edge agrees, and a line
/// every segment starts at is also a line of the interval's own grid.
pub(crate) fn segment_length(interval: Duration) -> Duration {
	let interval = interval.as_nanos().max(1);
	let length = MIN_SEGMENT.as_nanos().div_ceil(interval).max(1) * interval;
	Duration::from_nanos(u64::try_from(length).unwrap_or(u64::MAX))
}

/// A segment's groups, by start, oldest first.
///
/// Starts are timestamps, so the groups a segment holds are listed rather than spanned: the keys
/// between two groups name none. A segment holding more groups than any request fetches (see
/// [`Rendition::max_groups`](super::rendition::Rendition::max_groups)) is never served, so past
/// that its groups are only counted: groups whose pts stalls in one segment cost no memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Groups {
	starts: Vec<u64>,
	/// The groups past [`MAX_LISTED`] it holds, counted rather than listed.
	unlisted: usize,
}

/// The most starts a segment lists: one more than any request fetches.
const MAX_LISTED: usize = super::rendition::MAX_SEGMENT_GROUPS as usize + 1;

impl Groups {
	/// The one group starting at `start`.
	pub fn one(start: u64) -> Self {
		Self {
			starts: vec![start],
			unlisted: 0,
		}
	}

	/// The first group's start, or `None` for a gap.
	pub fn first(&self) -> Option<u64> {
		self.starts.first().copied()
	}

	/// The last group's start listed, or `None` for a gap.
	pub fn last(&self) -> Option<u64> {
		self.starts.last().copied()
	}

	/// How many groups the segment holds, listed or not.
	pub fn len(&self) -> usize {
		self.starts.len() + self.unlisted
	}

	/// The groups starting at or below `last`.
	pub fn through(&self, last: u64) -> Self {
		Self {
			starts: self.starts.iter().copied().take_while(|start| *start <= last).collect(),
			unlisted: 0,
		}
	}

	/// Each group's start, oldest first.
	pub fn starts(&self) -> std::vec::IntoIter<u64> {
		self.starts.clone().into_iter()
	}

	/// Add a group starting after every one held, counting it once the list is full.
	fn push(&mut self, start: u64) {
		match self.starts.len() < MAX_LISTED {
			true => self.starts.push(start),
			false => self.unlisted += 1,
		}
	}
}

/// One segment the grid completed, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
	/// A segment holding groups.
	Media {
		/// The segment's number on the grid.
		segment: u64,
		/// Its groups, by start.
		groups: Groups,
		/// Its first group's place on the grid.
		pts: Timestamp,
		/// Its first group's first sample's pts, where its media starts.
		media: Timestamp,
		/// Where its `EXTINF` starts: its first group's pts, or its line when that group starts
		/// after it, as a group resuming from a pause in the slot it lands in does. The time from the
		/// line to the group heads the segment, as a gap's does, rather than trailing the segment
		/// before it.
		start: Timestamp,
		/// Where its `EXTINF` ends: the next segment's start.
		end: Timestamp,
		/// Where its last group's content ends, the next group's first frame or its own end, when
		/// known: a fallback for timing its last sample.
		content: Option<Timestamp>,
		/// Whether every group of its slot is known: the grid placed a group in an earlier slot
		/// first, or its first group starts at or before its line, so no earlier group shares it.
		/// A segment whose first group starts after its line may follow earlier groups of the same
		/// segment that only another edge saw.
		whole: bool,
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

/// A group seen but not yet placed: placing it needs its end, unless one of its frames passes the
/// next line first.
#[derive(Clone, Copy, Debug)]
struct Group {
	start: u64,
	/// Where the grid places it.
	pts: Timestamp,
	/// Its first frame's pts, where the media before it ends.
	media: Timestamp,
}

/// The segment collecting groups, completed once a group lands in a later one or its newest group
/// ends past the next line.
#[derive(Clone, Debug)]
struct Open {
	segment: u64,
	groups: Groups,
	pts: Timestamp,
	media: Timestamp,
	whole: bool,
	/// Where a discontinuity marker after its last group stopped the content, if one did.
	stop: Option<Timestamp>,
}

/// Places one rendition's groups, fed in start order, into segments on a grid of one length.
pub(crate) struct Grid {
	/// The segment length in nanoseconds, at least one.
	length: u128,
	/// How far past a line a group may start and still start that line's segment, in nanoseconds.
	slack: u128,
	/// The newest group, until its end places it.
	pending: Option<Group>,
	open: Option<Open>,
	/// The segment completed by its newest group's end while no later group has started yet: the
	/// gaps up to the next group's follow it.
	closed: Option<u64>,
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
		let length = length.as_nanos().max(1);
		Self {
			length,
			slack: length / SLACK,
			pending: None,
			open: None,
			closed: None,
			last: None,
			max_gaps: max_gaps as u64,
		}
	}

	/// The segment a group starting at `pts` and ending at `end` belongs to, or `None` while it
	/// needs an `end` it doesn't have.
	pub fn segment(&self, pts: Timestamp, end: Option<Timestamp>) -> Option<u64> {
		let pts = super::nanos(pts);
		let slot = pts / self.length;
		// Within the slack past a line, it starts that line's segment whatever it reaches.
		if pts - slot * self.length <= self.slack {
			return Some(slot as u64);
		}
		let line = (slot + 1) * self.length;
		let end = super::nanos(end?);
		Some(if end > line { slot + 1 } else { slot } as u64)
	}

	/// The open segment, which a later group, its newest group's end, or the track's end completes.
	pub fn open(&self) -> Option<u64> {
		self.open.as_ref().map(|open| open.segment)
	}

	/// The groups of the open segment so far.
	pub fn open_groups(&self) -> Option<Groups> {
		self.open.as_ref().map(|open| open.groups.clone())
	}

	/// The oldest group no completed segment holds: the open segment's first, else the group
	/// waiting for its end.
	pub fn unplaced(&self) -> Option<u64> {
		let open = self.open.as_ref().and_then(|open| open.groups.first());
		open.or(self.pending.map(|group| group.start))
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
		if super::nanos(pts) > super::nanos(line) {
			line
		} else {
			pts
		}
	}

	/// The pts of line `segment`.
	fn line(&self, segment: u64) -> Timestamp {
		let nanos = (segment as u128).saturating_mul(self.length);
		Timestamp::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)).unwrap_or(Timestamp::ZERO)
	}

	/// The group keyed `start` is placed at `pts`, after every group already fed, and its first
	/// frame is at `media`, which ends the content before it. Returns the segments it completes.
	pub fn group(&mut self, start: u64, pts: Timestamp, media: Timestamp) -> Vec<Slot> {
		let mut out = Vec::new();
		// A group starting before the one before it, even within one slot, can't share a segment
		// or a numbering with what came before, since its media would run backwards.
		if self.last.replace(pts).is_some_and(|last| pts < last) {
			self.pending = None;
			self.open = None;
			self.closed = None;
			out.push(Slot::Reset);
		}
		if let Some(pending) = self.pending.take() {
			let segment = self.segment(pending.pts, Some(pts)).expect("bounded");
			self.place(pending, segment, &mut out);
		}
		let group = Group { start, pts, media };
		// A group starting on a line, or within the slack past one, needs no end to place, so the
		// segment before it completes now.
		match self.segment(pts, None) {
			Some(segment) => self.place(group, segment, &mut out),
			None => self.pending = Some(group),
		}
		out
	}

	/// A frame of the newest group starts at `pts`. Once that is past the line after the group's
	/// start, the group contains the line, so it is placed in that line's segment now rather than
	/// when its end is known: the same segment its end would give it. Returns the segments it
	/// completes.
	pub fn reach(&mut self, pts: Timestamp) -> Vec<Slot> {
		let mut out = Vec::new();
		let Some(pending) = self.pending else {
			return out;
		};
		let slot = super::nanos(pending.pts) / self.length;
		if super::nanos(pts) > (slot + 1) * self.length {
			self.pending = None;
			self.place(pending, slot as u64 + 1, &mut out);
		}
		out
	}

	/// The newest group ends at `at`, as its GROUP_END says, so no later group starts before it.
	/// Completes its segment once `at` reaches the next line, since every later group then starts a
	/// later segment: the segment its successor would complete. A group waiting to be placed is left
	/// to its successor or its frames, as on an edge whose GROUP_END arrives after the successor.
	/// Returns the segments it completes.
	pub fn end(&mut self, at: Timestamp) -> Vec<Slot> {
		let mut out = Vec::new();
		if self.pending.is_some() {
			return out;
		}
		let reached =
			(self.open.as_ref()).is_some_and(|open| super::nanos(at) >= super::nanos(self.line(open.segment + 1)));
		if reached && let Some(open) = self.open.take() {
			let segment = open.segment;
			let end = self.line(segment + 1);
			let content = Some(open.stop.unwrap_or(at));
			out.push(self.media(open, end, content));
			self.closed = Some(segment);
		}
		out
	}

	/// Whether the newest group waits for its end to be placed, which [`reach`](Self::reach) may
	/// settle early.
	pub fn pending(&self) -> bool {
		self.pending.is_some()
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
				.find(|end| super::nanos(*end) > super::nanos(open.pts))
				.unwrap_or(open.pts);
			out.push(self.media(open, end, Some(end)));
		}
		out
	}

	/// Where segment `next` starts when the next group placed starts at `pts` in `segment`: there,
	/// or at its line when it is a gap before it.
	fn boundary(&self, next: u64, segment: u64, pts: Timestamp) -> Timestamp {
		match next == segment {
			true => self.start(segment, pts),
			false => self.line(next),
		}
	}

	/// The completed segment `open`, its `EXTINF` ending at `end` and its content at `content`.
	fn media(&self, open: Open, end: Timestamp, content: Option<Timestamp>) -> Slot {
		Slot::Media {
			segment: open.segment,
			start: self.start(open.segment, open.pts),
			groups: open.groups,
			pts: open.pts,
			media: open.media,
			end,
			content,
			whole: open.whole,
		}
	}

	/// Place `group` in `segment`, completing the open segment and the gaps before it when it is a
	/// later one.
	fn place(&mut self, group: Group, segment: u64, out: &mut Vec<Slot>) {
		// The newest segment before this group's, if the grid saw one since it last reset.
		let before = match self.open.take() {
			Some(mut open) if segment == open.segment => {
				open.groups.push(group.start);
				// The content resumed within the segment, so it runs to whatever ends this group.
				open.stop = None;
				self.open = Some(open);
				return;
			}
			Some(open) if segment > open.segment => {
				let previous = open.segment;
				let content = Some(open.stop.unwrap_or(group.media));
				let end = self.boundary(previous + 1, segment, group.pts);
				out.push(self.media(open, end, content));
				Some(previous)
			}
			Some(_) => {
				out.push(Slot::Reset);
				None
			}
			None => match self.closed.take() {
				Some(closed) if segment <= closed => {
					out.push(Slot::Reset);
					None
				}
				closed => closed,
			},
		};
		if let Some(before) = before {
			let first = (before + 1).max(segment.saturating_sub(self.max_gaps));
			for gap in first..segment {
				out.push(Slot::Gap {
					segment: gap,
					pts: self.line(gap),
					end: self.boundary(gap + 1, segment, group.pts),
				});
			}
		}
		// Seen first, it starts its segment only if it starts at or before the line, since a group
		// starting after the line may follow earlier ones of the same segment.
		let whole = before.is_some() || super::nanos(group.pts) <= super::nanos(self.line(segment));
		self.open = Some(Open {
			segment,
			groups: Groups::one(group.start),
			pts: group.pts,
			media: group.media,
			whole,
			stop: None,
		});
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// A segment that piles up more groups than any request fetches counts the rest, so its
	/// memory stays bounded however long its groups' pts stalls, and it still reads as too long.
	#[test]
	fn an_overfull_segment_counts_its_groups_past_the_list() {
		let mut groups = Groups::one(0);
		for start in 1..(MAX_LISTED as u64 + 10) {
			groups.push(start);
		}
		assert_eq!(groups.len(), MAX_LISTED + 10);
		assert_eq!(groups.starts.len(), MAX_LISTED);
		assert!(groups.len() as u64 > super::super::rendition::MAX_SEGMENT_GROUPS);
	}

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
				} => (
					segment,
					groups.first().unwrap()..groups.last().unwrap() + 1,
					start.as_millis().unwrap() as u64,
					end.as_millis().unwrap() as u64,
				),
				Slot::Gap { segment, pts, end } => (
					segment,
					0..0,
					pts.as_millis().unwrap() as u64,
					end.as_millis().unwrap() as u64,
				),
				Slot::Reset => panic!("unexpected reset"),
			})
			.collect()
	}

	/// Feed groups starting at `starts` ms, numbered from zero, then end the track at `end` ms.
	fn run(length_ms: u64, starts: &[u64], end: u64) -> Vec<Listed> {
		let mut grid = Grid::new(Duration::from_millis(length_ms), 256);
		let mut slots = Vec::new();
		for (sequence, start) in starts.iter().enumerate() {
			slots.extend(grid.group(sequence as u64, ms(*start), ms(*start)));
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
				&[200, 5990],
				5995,
				&[(1, 0..1, 200, 4000), (2, 1..2, 4000, 5995)],
			),
			(
				"a group starting just past a line starts that line's segment",
				2000,
				&[0, 2030, 4000],
				6000,
				&[(0, 0..1, 0, 2000), (1, 1..2, 2000, 4000), (2, 2..3, 4000, 6000)],
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
						slots.extend(grid.group(sequence as u64, ms(*start), ms(*start)));
					}
					slots.extend(grid.finish(Some(ms(end - 1)), Some(ms(end))));
					// Frames read before the next group starts, in any order, list the same
					// segments, only sooner.
					let mut early = Grid::new(Duration::from_millis(length), 256);
					let mut early_slots = Vec::new();
					for (sequence, start) in starts.iter().enumerate().skip(join) {
						early_slots.extend(early.group(sequence as u64, ms(*start), ms(*start)));
						let until = starts.get(sequence + 1).copied().unwrap_or(end);
						for _ in 0..3 {
							early_slots.extend(early.reach(ms(start + next(until - start))));
						}
					}
					early_slots.extend(early.finish(Some(ms(end - 1)), Some(ms(end))));
					let early = listed(early_slots);
					// Each group's GROUP_END, anywhere up to where its successor starts, lists the
					// same segments too, so an edge's segments never depend on whether it arrives
					// before the successor.
					let mut ended = Grid::new(Duration::from_millis(length), 256);
					let mut ended_slots = Vec::new();
					for (sequence, start) in starts.iter().enumerate().skip(join) {
						ended_slots.extend(ended.group(sequence as u64, ms(*start), ms(*start)));
						let until = starts.get(sequence + 1).copied().unwrap_or(end);
						ended_slots.extend(ended.end(ms(start + next(until - start + 1))));
					}
					ended_slots.extend(ended.finish(Some(ms(end - 1)), Some(ms(end))));
					let ended = listed(ended_slots);
					let listed = listed(slots);
					assert_eq!(early, listed, "{interval} ms: {starts:?}");
					assert_eq!(ended, listed, "{interval} ms: {starts:?}");
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

	/// The segments of keyframes at microsecond `starts`, as `(segment, groups held)`, and the gaps'
	/// numbers.
	fn placed(length: Duration, starts: &[u64]) -> (Vec<(u64, usize)>, Vec<u64>) {
		let at = |micros: u64| Timestamp::from_micros(micros).unwrap();
		let mut grid = Grid::new(length, 256);
		let mut slots = Vec::new();
		for (sequence, start) in starts.iter().enumerate() {
			slots.extend(grid.group(sequence as u64, at(*start), at(*start)));
		}
		let end = starts.last().unwrap() + 1;
		slots.extend(grid.finish(Some(at(end)), Some(at(end))));
		let mut segments = Vec::new();
		let mut gaps = Vec::new();
		for slot in slots {
			match slot {
				Slot::Media { segment, groups, .. } => segments.push((segment, groups.len())),
				Slot::Gap { segment, .. } => gaps.push(segment),
				Slot::Reset => panic!("unexpected reset"),
			}
		}
		(segments, gaps)
	}

	/// A 29.97 fps encoder keying a frame next to each 2 s line lands each keyframe up to a frame
	/// either side of it: every segment still holds one GOP, with no gap between them.
	#[test]
	fn ntsc_keyframes_either_side_of_a_line_fill_every_segment() {
		let frame = |index: u64| index * 1_001_000_000 / 30_000;
		// The first frame at or past each line, or every third line the one before it.
		let key = |line: u64| {
			(line * 2_000_000 * 30_000).div_ceil(1_001_000_000) - u64::from(line.is_multiple_of(3) && line > 0)
		};
		let starts: Vec<u64> = (0..200).map(|line| frame(key(line))).collect();
		assert!(
			starts
				.iter()
				.enumerate()
				.any(|(line, start)| *start > line as u64 * 2_000_000)
		);
		assert!(
			starts
				.iter()
				.enumerate()
				.any(|(line, start)| *start < line as u64 * 2_000_000)
		);
		let (segments, gaps) = placed(Duration::from_secs(2), &starts);
		assert!(gaps.is_empty(), "gaps at {gaps:?}");
		assert_eq!(segments, (0..200).map(|segment| (segment, 1)).collect::<Vec<_>>());
	}

	/// A 60-frame GOP at 29.97 fps lasts 2,002 ms, so on a 2 s grid each keyframe lands 2 ms later
	/// than the one before: the first two minutes fill every segment, and the drift leaves one
	/// slot to a gap only once it outgrows the slack, a gap as long as that drift.
	#[test]
	fn ntsc_gops_on_a_shorter_grid_skip_a_slot_once_per_cycle() {
		let starts: Vec<u64> = (0..2_500).map(|gop| gop * 2_002_000).collect();
		let (segments, gaps) = placed(Duration::from_secs(2), &starts[..60]);
		assert!(gaps.is_empty(), "gaps at {gaps:?}");
		assert!(segments.iter().all(|(_, groups)| *groups == 1));

		let mut grid = Grid::new(Duration::from_secs(2), 256);
		let mut slots = Vec::new();
		for (sequence, start) in starts.iter().enumerate() {
			slots.extend(grid.group(
				sequence as u64,
				Timestamp::from_micros(*start).unwrap(),
				Timestamp::from_micros(*start).unwrap(),
			));
		}
		let gaps: Vec<u64> = slots
			.iter()
			.filter_map(|slot| match slot {
				Slot::Gap { pts, end, .. } => Some((end.as_millis().unwrap() - pts.as_millis().unwrap()) as u64),
				_ => None,
			})
			.collect();
		// One slot in each 1,000 GOPs.
		assert_eq!(gaps.len(), 3, "{gaps:?}");
		assert!(gaps.iter().all(|gap| *gap > 120), "millisecond gaps: {gaps:?}");
		assert!(slots.iter().all(|slot| match slot {
			Slot::Media { groups, .. } => groups.len() == 1,
			_ => true,
		}));
	}

	/// A group's GROUP_END completes its segment at once when it reaches the next line, since no
	/// later group can join it, but not when it ends short of the line.
	#[test]
	fn an_end_at_the_next_line_completes_the_segment() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(0), ms(0)).is_empty());
		assert!(
			grid.end(ms(1500)).is_empty(),
			"a later group may still start before 2 s"
		);
		// Within the slack, so placed in segment 0 at once.
		assert!(grid.group(1, ms(100), ms(100)).is_empty());
		assert_eq!(listed(grid.end(ms(2000))), [(0, 0..2, 0, 2000)]);
		assert!(!grid.waiting(), "nothing is left open");
		// A pause: the next group lists the gaps after the segment its end completed.
		assert_eq!(
			listed(grid.group(2, ms(6050), ms(6050))),
			[(1, 0..0, 2000, 4000), (2, 0..0, 4000, 6000)]
		);
		assert_eq!(listed(grid.end(ms(8000))), [(3, 2..3, 6000, 8000)]);

		// A group off the grid waits for its successor whatever its end: one ending at 1.5 s and
		// followed at 2.5 s contains the line at 2 s, as an edge seeing the successor first places it.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(1000), ms(1000)).is_empty());
		assert!(grid.end(ms(1500)).is_empty());
		assert!(grid.pending());
		assert!(grid.group(1, ms(2500), ms(2500)).is_empty());
		assert_eq!(grid.open(), Some(1));

		// A later group starting inside a completed segment resets rather than rewriting it.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0), ms(0));
		grid.end(ms(2000));
		assert_eq!(grid.group(1, ms(100), ms(100)), [Slot::Reset]);
	}

	/// An edge seeing a segment's first group start after its line can't tell no earlier group
	/// shares the segment, so that segment isn't whole; the next one is, since this edge saw the
	/// group before it.
	#[test]
	fn a_segment_is_whole_once_its_first_group_is_known_first() {
		let whole = |slots: Vec<Slot>| -> Vec<bool> {
			slots
				.into_iter()
				.filter_map(|slot| match slot {
					Slot::Media { whole, .. } => Some(whole),
					_ => None,
				})
				.collect()
		};
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(30), ms(30));
		assert_eq!(whole(grid.group(1, ms(2030), ms(2030))), [false]);
		assert_eq!(whole(grid.group(2, ms(4030), ms(4030))), [true]);

		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(1990), ms(1990));
		assert_eq!(whole(grid.group(1, ms(4000), ms(4000))), [true], "it contains its line");
	}

	/// The maintainer's 2026-11-03 rule: an edge joining at B numbers it 3, as one that saw A does.
	#[test]
	fn a_late_join_numbers_like_an_early_one() {
		let early = run(2000, &[0, 7000, 7200], 7400);
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		let mut late = grid.group(1, ms(7000), ms(7000));
		late.extend(grid.group(2, ms(7200), ms(7200)));
		late.extend(grid.finish(Some(ms(7400)), Some(ms(7400))));
		assert_eq!(listed(late), [(3, 1..3, 6000, 7400)]);
		assert_eq!(early.last(), Some(&(3, 1..3, 6000, 7400)));
	}

	/// A group off the grid waits for its successor; one on a line is placed at once.
	#[test]
	fn a_segment_completes_once_a_later_one_starts() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(0), ms(0)).is_empty());
		assert!(
			grid.group(1, ms(1000), ms(1000)).is_empty(),
			"its successor may still reach the next line"
		);
		assert_eq!(listed(grid.group(2, ms(2000), ms(2000))), [(0, 0..2, 0, 2000)]);
		assert!(grid.group(3, ms(3500), ms(3500)).is_empty());
		assert_eq!(
			listed(grid.group(4, ms(4500), ms(4500))),
			[(1, 2..3, 2000, 3500)],
			"3.5s reaches 4s"
		);

		// A frame past the next line settles the newest group without its successor.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(500), ms(500)).is_empty());
		assert!(
			grid.reach(ms(1900)).is_empty(),
			"short of the line, it may still end before it"
		);
		assert!(
			grid.reach(ms(2000)).is_empty(),
			"its successor may still start on the line"
		);
		assert!(
			grid.reach(ms(2033)).is_empty(),
			"it opens segment 1, which nothing completes yet"
		);
		assert!(!grid.pending());
		assert!(grid.group(1, ms(2500), ms(2500)).is_empty());
		assert_eq!(
			listed(grid.reach(ms(4100))),
			[(1, 0..1, 500, 2500)],
			"listed at 4.1s rather than when group 2 starts at 4.5s"
		);
		assert!(grid.reach(ms(4200)).is_empty(), "already placed");
		assert!(grid.group(2, ms(4500), ms(4500)).is_empty());
		assert_eq!(
			listed(grid.finish(Some(ms(5000)), Some(ms(5000)))),
			[(2, 1..3, 2500, 5000)]
		);
	}

	/// A long pause lists only the newest gaps, the ones a playlist window can still hold.
	#[test]
	fn a_long_pause_lists_at_most_the_newest_gaps() {
		let mut grid = Grid::new(Duration::from_secs(2), 3);
		grid.group(0, ms(0), ms(0));
		let slots = listed(grid.group(1, ms(3_600_000), ms(3_600_000)));
		assert_eq!(slots.len(), 4);
		assert_eq!(slots[0], (0, 0..1, 0, 2000));
		assert_eq!(slots[1].0, 1797);
		assert_eq!(slots[3], (1799, 0..0, 3_598_000, 3_600_000));
	}

	/// A group going back within the open slot resets too: its media can't follow what it precedes.
	#[test]
	fn a_rewind_within_a_slot_resets() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		assert!(grid.group(0, ms(10_500), ms(10_500)).is_empty());
		assert_eq!(grid.group(1, ms(10_200), ms(10_200)), [Slot::Reset]);
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
					Slot::Media { content, .. } => Some(content.map(|at| at.as_millis().unwrap() as u64)),
					_ => None,
				})
				.collect()
		};
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0), ms(0));
		grid.group(1, ms(1000), ms(1000));
		assert!(content(grid.stop(ms(1500))).is_empty(), "the slot is still open");
		assert_eq!(content(grid.group(2, ms(8000), ms(8000))), [Some(1500)]);

		// The track ending after a marker ends at the marker.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0), ms(0));
		grid.stop(ms(700));
		assert_eq!(listed(grid.finish(None, None)), [(0, 0..1, 0, 700)]);

		// Content resuming within the slot runs on to whatever ends it.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0), ms(0));
		grid.stop(ms(300));
		grid.group(1, ms(1000), ms(1000));
		assert_eq!(content(grid.group(2, ms(2000), ms(2000))), [Some(2000)]);

		// A group placed at a start below its first frame ends the content before it at that frame,
		// not at its start: the last sample keeps its whole duration.
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(0), ms(100));
		assert_eq!(content(grid.group(2000, ms(2000), ms(2100))), [Some(2100)]);
	}

	/// A group going back past the open slot resets the numbering.
	#[test]
	fn a_rewind_resets() {
		let mut grid = Grid::new(Duration::from_secs(2), 256);
		grid.group(0, ms(10_000), ms(10_000));
		assert_eq!(grid.group(1, ms(0), ms(0)), [Slot::Reset]);
		assert_eq!(
			listed(grid.finish(Some(ms(2000)), Some(ms(2000)))),
			[(0, 1..2, 0, 2000)]
		);
	}
}
