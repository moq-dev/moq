//! A subscriber's own ranges, resolved to the group sequences it reads.
//!
//! The aggregate a publisher sees is the union of every subscriber's ranges, so it says
//! what arrives, not what one subscriber asked for. A [`Window`] is the per-subscriber
//! side: resolved at the hop serving the subscriber, it filters that subscriber's reads.

use std::ops;

use rangemap::RangeInclusiveSet;

use super::{Anchor, Range};
use crate::{Error, Result, Timestamp};

/// One range resolved to groups: from `start` up to the exclusive `end`, `None` while it
/// stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) struct Resolved {
	pub start: u64,
	pub end: Option<u64>,
}

/// When a serving hop's groups start, for resolving time anchors.
///
/// Each query answers from ordered indexes, so a time end that walks them forward a step
/// at a time pays for each group or run it passes once, however long it stays unsettled.
pub(in crate::model) trait Holdings {
	/// Whether the track carries timestamps at all.
	fn timed(&self) -> bool;

	/// Whether the groups are a copy received over a wire that can't resolve relative or
	/// time anchors as the publisher would, so only what [`Range::is_wire`] covers reads.
	fn drops_anchors(&self) -> bool {
		false
	}

	/// Where `pts` falls among the groups with a known start.
	fn containing(&self, pts: Timestamp) -> Containing;

	/// The lowest group at or above `from` known to start, with where it starts.
	fn next_start(&self, from: u64) -> Option<(u64, Timestamp)>;

	/// The lowest held group at or above `from`, with what it says about its start.
	fn next_held(&self, from: u64) -> Option<(u64, Held)>;

	/// The exclusive end of the run of sequences from `sequence` that can no longer arrive
	/// (dropped, given up, or below where the feed starts), `None` while `sequence` may.
	fn gone(&self, sequence: u64) -> Option<u64>;

	/// Whether nothing more can arrive or start here, so a time end has nothing to wait
	/// on and is left uncapped.
	fn complete(&self) -> bool;
}

/// Where a presentation time falls among the groups a hop holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Containing {
	/// The newest group starting at or before it.
	Group(u64),
	/// Every group with a known start begins after it.
	Before,
	/// No group has a known start.
	Unknown,
}

/// What a held group says about where it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) enum Held {
	/// Its frame 0 presented at this time.
	Started(Timestamp),
	/// It holds frame 0 and has not presented it yet.
	Pending,
	/// It never will: it ended empty, was aborted, or is held from past frame 0.
	Never,
}

/// A hop that holds no groups of its own, such as a reader following route copies that
/// resolve its ranges for it: a time end there filters nothing.
pub(in crate::model) struct Unheld;

impl Holdings for Unheld {
	fn timed(&self) -> bool {
		true
	}

	fn containing(&self, _: Timestamp) -> Containing {
		Containing::Unknown
	}

	fn next_start(&self, _: u64) -> Option<(u64, Timestamp)> {
		None
	}

	fn next_held(&self, _: u64) -> Option<(u64, Held)> {
		None
	}

	fn gone(&self, _: u64) -> Option<u64> {
		None
	}

	fn complete(&self) -> bool {
		true
	}
}

/// Whether `time` is strictly after `other`, as instants: exact across scales, unlike
/// [`Timestamp`]'s `Ord`, which breaks a tie between scales.
pub(in crate::model) fn later(time: Timestamp, other: Timestamp) -> bool {
	u128::from(time.value()) * u128::from(other.scale().as_u64())
		> u128::from(other.value()) * u128::from(time.scale().as_u64())
}

/// A subscriber's [`Range`]s resolved to the group sequences it reads.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, so only replacing the ranges resolves again.
	ranges: Vec<Range>,
	spans: Vec<Span>,
	/// The groups the spans cover, an unsettled time end through its cap.
	groups: RangeInclusiveSet<u64>,
	/// The part of `groups` a miss may ask for: see [`Self::missable`].
	missable: RangeInclusiveSet<u64>,
	/// A relative anchor waits on a first group, so nothing is filtered yet.
	pending: bool,
	/// Nothing is left to resolve: see [`Self::is_settled`]. Kept so a read checks it once
	/// per group, not once per range.
	settled: bool,
}

/// One non-empty range, resolved.
#[derive(Clone, Copy, Debug)]
struct Span {
	/// Which of the ranges it came from.
	index: usize,
	start: u64,
	end: End,
	/// Whether the start names a group, rather than reaching back to the oldest one.
	floored: bool,
	/// The lowest group a miss asks for. A time start before every group held asks none
	/// below the oldest: which of them it reaches is unknown until they start.
	reach: u64,
}

impl Span {
	/// The exclusive end of the groups read now, `None` for none: an unsettled time end
	/// reads up to its cap.
	fn limit(&self) -> Option<u64> {
		match self.end {
			End::Before(end) => Some(end),
			End::Open => None,
			End::Time(time) => time.limit,
		}
	}

	/// Whether group `sequence` is read now.
	fn covers(&self, sequence: u64) -> bool {
		sequence >= self.start && self.limit().is_none_or(|end| sequence < end)
	}
}

#[derive(Clone, Copy, Debug)]
enum End {
	/// Before this group.
	Before(u64),
	/// Past every group.
	Open,
	/// Before the first group starting after a time, once that is known.
	Time(TimeEnd),
}

/// A time end not settled yet, and how far the walk to settle it got.
///
/// Timestamps never rewind, so the end falls before the lowest group starting past `pts`,
/// once every sequence between it and the newest one starting at or before `pts` is
/// accounted for. Every position below only moves forward, so each group or run of
/// sequences is passed once, whatever a peer leaves missing.
#[derive(Clone, Copy, Debug)]
struct TimeEnd {
	pts: Timestamp,
	/// The newest group known to start at or before `pts`: it and everything below it in
	/// the span is in range.
	known: Option<u64>,
	/// The lowest group known to start after `pts`: where the end falls once settled.
	after: Option<u64>,
	/// Where the walk over held groups resumes: those below it started, or never will.
	held: u64,
	/// Where settling resumes: every sequence from above `known` up to it is accounted for.
	accounted: u64,
	/// The exclusive cap on what is read meanwhile, `None` for none.
	limit: Option<u64>,
	/// The group the cap admits on speculation: the first above `known` that may still
	/// start, which may start past `pts`.
	speculative: Option<u64>,
}

impl TimeEnd {
	fn new(pts: Timestamp, start: u64) -> Self {
		Self {
			pts,
			known: None,
			after: None,
			held: start,
			accounted: start,
			limit: Some(start),
			speculative: None,
		}
	}

	/// The lowest sequence not known to be in range, `None` once the last sequence is.
	fn unknown(&self, start: u64) -> Option<u64> {
		match self.known {
			Some(known) => known.checked_add(1).map(|next| next.max(start)),
			None => Some(start),
		}
	}

	/// Walk the held groups on from where the last call stopped. Returns where the end
	/// falls once it settles.
	fn advance(&mut self, start: u64, holdings: &impl Holdings) -> Option<u64> {
		// The starts index passes a group that may still start, so a stalled one never
		// keeps a later group known to start inside the end from being read.
		while let Some(from) = self.unknown(start)
			&& let Some((sequence, at)) = holdings.next_start(from)
			&& self.after.is_none_or(|after| sequence < after)
		{
			match later(at, self.pts) {
				true => self.after = Some(sequence),
				false => self.known = Some(sequence),
			}
		}

		// The first sequence above `known` that may still start is the one read on
		// speculation: a held group yet to start, or a missing one that may arrive. Only a
		// group starting inside the end is jumped to, since it puts all below it in range.
		self.speculative = None;
		while let Some(from) = self.unknown(start) {
			let at = self.held.max(from);
			if self.after.is_some_and(|after| at >= after) {
				break;
			}
			let next = holdings
				.next_held(at)
				.filter(|(sequence, _)| self.after.is_none_or(|after| *sequence < after));
			match next {
				Some((sequence, held)) if sequence == at => match held {
					Held::Started(time) if later(time, self.pts) => self.after = Some(at),
					Held::Started(_) => self.known = Some(at),
					Held::Never => {}
					Held::Pending => {
						self.held = at;
						self.speculative = Some(at);
						break;
					}
				},
				Some((sequence, Held::Started(time))) if !later(time, self.pts) => self.known = Some(sequence),
				// Past every held group: the next to arrive is the speculative one, judged
				// once a settle sees it, before it is read.
				None if self.after.is_none() => {
					self.held = at;
					break;
				}
				_ => match holdings.gone(at) {
					Some(end) if end > at => {
						self.held = end;
						continue;
					}
					_ => {
						self.held = at;
						self.speculative = (!holdings.complete()).then_some(at);
						break;
					}
				},
			}
			// Nothing sits above the last sequence, so the walk ends on it.
			let Some(next) = at.checked_add(1) else { break };
			self.held = next;
		}
		self.limit = match (self.speculative, self.after) {
			_ if holdings.complete() => None,
			(Some(speculative), _) => speculative.checked_add(1),
			(None, Some(after)) => Some(after),
			// Every held group is accounted for: the next to arrive is the speculative one.
			(None, None) => None,
		};

		// Settle once every sequence up to the first group starting past `pts` can no
		// longer start inside it.
		//
		// Datagrams stay ephemeral and never index where their group starts, so a sequence
		// that arrived only as a datagram counts as gone here. A stream copy of that group
		// arriving later is read even if it starts past `pts`: accepted, since a publisher
		// that streams a group it also sent as a datagram is rare.
		let after = self.after?;
		// Everything at or below `known` is in range, and `after` lies above it.
		self.accounted = self.accounted.max(self.unknown(start).unwrap_or(after));
		while self.accounted < after {
			let next = holdings.next_held(self.accounted);
			match next {
				Some((sequence, held)) if sequence == self.accounted => match held {
					Held::Pending => return None,
					// It started past `pts` since the walk above stopped on it: everything
					// below is accounted for, so the end falls here.
					Held::Started(at) if later(at, self.pts) => {
						self.after = Some(sequence);
						return self.after;
					}
					_ => self.accounted += 1,
				},
				_ => {
					let below = next.map_or(after, |(sequence, _)| sequence.min(after));
					self.accounted = match holdings.complete() {
						true => below,
						false => holdings.gone(self.accounted)?.min(below),
					};
				}
			}
		}
		self.after
	}
}

impl Window {
	/// Resolve `ranges`, counting relative anchors back from `latest`.
	///
	/// Refuses a time anchor on an untimed track with [`Error::TimestampMismatch`], and an
	/// anchor a received copy's wire drops with [`Error::Unsupported`].
	pub fn new(ranges: &[Range], latest: Option<u64>, holdings: &impl Holdings) -> Result<Self> {
		let mut window = Self {
			ranges: ranges.to_vec(),
			spans: Vec::new(),
			groups: RangeInclusiveSet::new(),
			missable: RangeInclusiveSet::new(),
			pending: false,
			settled: false,
		};
		window.place(latest, holdings)?;
		Ok(window)
	}

	/// Whether nothing is left to resolve as groups arrive and start.
	pub fn is_settled(&self) -> bool {
		self.settled
	}

	/// Whether a relative anchor still waits on a first group.
	pub fn is_pending(&self) -> bool {
		self.pending
	}

	/// Resolve what the groups now held settle: relative anchors once `latest` exists, and
	/// time ends as groups start. Returns whether the groups in range moved.
	pub fn settle(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> bool {
		if self.pending {
			if latest.is_none() {
				return false;
			}
			// Checked when the ranges were first resolved, against the same track.
			let _ = self.place(latest, holdings);
			return !self.pending;
		}
		let mut moved = false;
		for span in &mut self.spans {
			let End::Time(time) = &mut span.end else { continue };
			let limit = time.limit;
			if let Some(end) = time.advance(span.start, holdings) {
				span.end = End::Before(end);
				moved = true;
			} else {
				moved |= time.limit != limit;
			}
		}
		if moved {
			self.rebuild();
		}
		moved
	}

	/// The groups an unsettled time end reads on speculation, which may start past it: a
	/// read parked on the cap wakes when one of them starts.
	pub fn speculative(&self) -> impl Iterator<Item = u64> + '_ {
		self.spans.iter().filter_map(|span| match span.end {
			End::Time(time) => time.speculative,
			_ => None,
		})
	}

	/// The lowest sequence whose start could still move an unsettled time end, `None`
	/// without one or once the last sequence is in range.
	pub fn watch_starts(&self) -> Option<u64> {
		self.spans
			.iter()
			.filter_map(|span| match span.end {
				End::Time(time) => time.unknown(span.start),
				_ => None,
			})
			.min()
	}

	/// Whether group `sequence` is in range. Everything is while a relative anchor waits.
	pub fn contains(&self, sequence: u64) -> bool {
		self.pending || self.groups.contains(&sequence)
	}

	/// Whether a datagram is in range, judged on its own and never through a time end's
	/// settling: its `timestamp` against each range's times, and its `sequence` against the
	/// range's absolute groups and the relative ones resolved so far.
	pub fn admits_datagram(&self, sequence: u64, timestamp: Option<Timestamp>) -> bool {
		let Some(timestamp) = timestamp else {
			return self.pending || self.spans.iter().any(|span| span.covers(sequence));
		};
		self.ranges
			.iter()
			.enumerate()
			.filter(|(_, range)| !range.is_empty())
			.any(|(index, range)| {
				// A relative anchor still waiting on a latest group bounds nothing yet.
				let span = self.spans.iter().find(|span| span.index == index);
				let started = match range.start {
					Anchor::Time(pts) => !later(pts, timestamp),
					Anchor::Absolute(start) => sequence >= start,
					Anchor::Relative(_) => span.is_none_or(|span| sequence >= span.start),
				};
				let ended = match range.end {
					None => true,
					Some(Anchor::Time(pts)) => !later(timestamp, pts),
					Some(Anchor::Absolute(end)) => sequence < end,
					Some(Anchor::Relative(_)) => {
						span.is_none_or(|span| !matches!(span.end, End::Before(end) if sequence >= end))
					}
				};
				started && ended
			})
	}

	/// The lowest group in range, `None` while a relative anchor waits. Nothing in range
	/// floors nowhere: group 0.
	pub fn floor(&self) -> Option<u64> {
		match self.pending {
			true => None,
			false => Some(self.groups.iter().next().map_or(0, |run| *run.start())),
		}
	}

	/// The lowest group the ranges name, `None` while one reaches back to the oldest group
	/// or a relative anchor waits on a first group.
	pub fn start(&self) -> Option<u64> {
		if self.pending || self.spans.iter().any(|span| !span.floored) {
			return None;
		}
		self.spans.iter().map(|span| span.start).min()
	}

	/// The lowest group a range names, `None` when none does or a relative anchor waits.
	/// Unlike [`Self::start`], a range reaching back to the oldest group names nothing.
	pub fn named(&self) -> Option<u64> {
		match self.pending {
			true => None,
			false => self
				.spans
				.iter()
				.filter(|span| span.floored)
				.map(|span| span.start)
				.min(),
		}
	}

	/// Each range resolved, in the order given: `None` for one that is empty or still
	/// waits on a first group. An unsettled time end is reported open, never at its cap.
	pub fn resolved(&self) -> Vec<Option<Resolved>> {
		let mut resolved = vec![None; self.ranges.len()];
		for span in &self.spans {
			resolved[span.index] = Some(Resolved {
				start: span.start,
				end: match span.end {
					End::Before(end) => Some(end),
					End::Open | End::Time(_) => None,
				},
			});
		}
		resolved
	}

	/// The exclusive end past every group in range, `None` when one stays open. An
	/// unsettled time end counts as open: its cap only holds reads back meanwhile.
	pub fn end(&self) -> Option<u64> {
		if self.pending || self.spans.iter().any(|span| matches!(span.end, End::Time(_))) {
			return None;
		}
		match self.groups.iter().next_back() {
			Some(run) => run.end().checked_add(1),
			None => Some(0),
		}
	}

	/// Whether `accounted` holds every group in range from `from` up to the exclusive `cap`,
	/// `false` while a range stays open (see [`Self::end`]).
	pub fn covered_by(&self, from: u64, cap: Option<u64>, accounted: &rangemap::RangeSet<u64>) -> bool {
		let Some(end) = self.end() else {
			return false;
		};
		let end = cap.map_or(end, |cap| cap.min(end));
		self.runs(from).take_while(|run| *run.start() < end).all(|run| {
			let run_end = run.end().checked_add(1).map_or(end, |next| next.min(end));
			accounted.gaps(&(*run.start()..run_end)).next().is_none()
		})
	}

	/// The groups in range from `from` on, lowest first.
	pub fn runs(&self, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
		self.from(&self.groups, from)
	}

	/// The groups in range from `from` on that a miss may ask for, lowest first: all of
	/// them but those below the oldest group held when a time start fell before it.
	pub fn missable(&self, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
		self.from(&self.missable, from)
	}

	fn from<'a>(
		&'a self,
		groups: &'a RangeInclusiveSet<u64>,
		from: u64,
	) -> impl Iterator<Item = ops::RangeInclusive<u64>> + 'a {
		let all = self.pending.then_some(from..=u64::MAX);
		let runs = (!self.pending).then(|| {
			groups
				.overlapping(from..=u64::MAX)
				.map(move |run| (*run.start()).max(from)..=*run.end())
		});
		all.into_iter().chain(runs.into_iter().flatten())
	}

	/// The first run of `range` that is in range.
	pub fn clip(&self, range: ops::Range<u64>) -> Option<ops::Range<u64>> {
		let last = range.end.checked_sub(1).filter(|last| *last >= range.start)?;
		if self.pending {
			return Some(range);
		}
		let run = self.groups.overlapping(range.start..=last).next()?;
		let end = match run.end().checked_add(1) {
			Some(end) => end.min(range.end),
			None => range.end,
		};
		Some((*run.start()).max(range.start)..end)
	}

	fn place(&mut self, latest: Option<u64>, holdings: &impl Holdings) -> Result<()> {
		let time = |anchor: Option<Anchor>| matches!(anchor, Some(Anchor::Time(_)));
		if !holdings.timed()
			&& self
				.ranges
				.iter()
				.any(|range| time(Some(range.start)) || time(range.end))
		{
			return Err(Error::TimestampMismatch);
		}
		if holdings.drops_anchors() && !self.ranges.iter().all(Range::is_wire) {
			return Err(Error::Unsupported);
		}
		self.spans.clear();
		self.pending = false;
		for (index, range) in self.ranges.iter().enumerate().filter(|(_, range)| !range.is_empty()) {
			let (Some((start, floored)), Some(end)) = (start(range.start, latest, holdings), end(range.end, latest))
			else {
				self.pending = true;
				continue;
			};
			let end = match end {
				End::Time(mut time) => {
					time = TimeEnd::new(time.pts, start);
					time.advance(start, holdings).map_or(End::Time(time), End::Before)
				}
				end => end,
			};
			let reach = match range.start {
				Anchor::Time(_) if !floored => holdings.next_start(0).map_or(start, |(oldest, _)| oldest),
				_ => start,
			};
			self.spans.push(Span {
				index,
				start,
				end,
				floored,
				reach,
			});
		}
		self.rebuild();
		Ok(())
	}

	fn rebuild(&mut self) {
		self.settled = !self.pending && !self.spans.iter().any(|span| matches!(span.end, End::Time(_)));
		self.groups = RangeInclusiveSet::new();
		self.missable = RangeInclusiveSet::new();
		for span in &self.spans {
			let last = match span.limit() {
				Some(end) => match end.checked_sub(1) {
					Some(last) if last >= span.start => last,
					_ => continue,
				},
				None => u64::MAX,
			};
			self.groups.insert(span.start..=last);
			if span.reach <= last {
				self.missable.insert(span.reach..=last);
			}
		}
	}
}

/// The first group a range starts at, and whether it names that group rather than
/// reaching back to the oldest one. `None` while it waits on a latest group.
fn start(anchor: Anchor, latest: Option<u64>, holdings: &impl Holdings) -> Option<(u64, bool)> {
	match anchor {
		Anchor::Absolute(start) => Some((start, true)),
		// Nothing sits below group 0, so the farthest offset needs no latest group.
		Anchor::Relative(u64::MAX) => Some((0, false)),
		Anchor::Relative(back) => Some((latest?.saturating_sub(back), true)),
		Anchor::Time(pts) => match holdings.containing(pts) {
			Containing::Group(group) => Some((group, true)),
			// The group holding it is older than any held, so reach back to the oldest.
			Containing::Before => Some((0, false)),
			// Nothing has started yet: like the live edge, the latest group.
			Containing::Unknown => Some((latest?, true)),
		},
	}
}

/// Where a range stops, or `None` while it waits on a latest group. A time end's walk
/// starts once its start is known.
fn end(anchor: Option<Anchor>, latest: Option<u64>) -> Option<End> {
	Some(match anchor {
		None => End::Open,
		Some(Anchor::Absolute(end)) => End::Before(end),
		Some(Anchor::Relative(back)) => latest?
			.saturating_sub(back)
			.checked_add(1)
			.map_or(End::Open, End::Before),
		Some(Anchor::Time(pts)) => End::Time(TimeEnd::new(pts, 0)),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::cell::Cell;

	fn at(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	/// A hop's groups by sequence, the runs that can no longer arrive, and how many held
	/// groups and starts a walk asked about.
	#[derive(Default)]
	struct Groups {
		held: std::collections::BTreeMap<u64, Held>,
		gone: Vec<ops::Range<u64>>,
		complete: bool,
		asked: Cell<usize>,
	}

	impl Groups {
		fn new(held: impl IntoIterator<Item = (u64, Held)>) -> Self {
			Self {
				held: held.into_iter().collect(),
				..Self::default()
			}
		}

		fn started(sequence: u64, millis: u64) -> (u64, Held) {
			(sequence, Held::Started(at(millis)))
		}
	}

	impl Holdings for Groups {
		fn timed(&self) -> bool {
			true
		}

		fn containing(&self, pts: Timestamp) -> Containing {
			let mut stamped = false;
			for (&sequence, held) in self.held.iter().rev() {
				let Held::Started(start) = held else { continue };
				stamped = true;
				if !later(*start, pts) {
					return Containing::Group(sequence);
				}
			}
			match stamped {
				true => Containing::Before,
				false => Containing::Unknown,
			}
		}

		fn next_start(&self, from: u64) -> Option<(u64, Timestamp)> {
			self.asked.set(self.asked.get() + 1);
			self.held.range(from..).find_map(|(&sequence, held)| match held {
				Held::Started(start) => Some((sequence, *start)),
				_ => None,
			})
		}

		fn next_held(&self, from: u64) -> Option<(u64, Held)> {
			self.asked.set(self.asked.get() + 1);
			self.held
				.range(from..)
				.next()
				.map(|(&sequence, held)| (sequence, *held))
		}

		fn gone(&self, sequence: u64) -> Option<u64> {
			self.gone
				.iter()
				.find(|gone| gone.contains(&sequence))
				.map(|gone| gone.end)
		}

		fn complete(&self) -> bool {
			self.complete
		}
	}

	struct Untimed;

	impl Holdings for Untimed {
		fn timed(&self) -> bool {
			false
		}

		fn containing(&self, _: Timestamp) -> Containing {
			Containing::Unknown
		}

		fn next_start(&self, _: u64) -> Option<(u64, Timestamp)> {
			None
		}

		fn next_held(&self, _: u64) -> Option<(u64, Held)> {
			None
		}

		fn gone(&self, _: u64) -> Option<u64> {
			None
		}

		fn complete(&self) -> bool {
			false
		}
	}

	fn time(millis: u64) -> Anchor {
		Anchor::Time(Timestamp::from_millis(millis).unwrap())
	}

	fn range(start: Anchor, end: impl Into<Option<Anchor>>) -> Range {
		Range { start, end: end.into() }
	}

	fn covered(window: &Window, upto: u64) -> Vec<u64> {
		(0..upto).filter(|sequence| window.contains(*sequence)).collect()
	}

	/// Disjoint ranges filter to exactly their groups; the gap between them is out.
	#[test]
	fn ranges_cover_only_their_groups() {
		let window = Window::new(&[Range::groups(2..4), Range::groups(7..)], Some(9), &Untimed).unwrap();
		assert_eq!(covered(&window, 10), vec![2, 3, 7, 8, 9]);
		assert_eq!(window.floor(), Some(2));
		assert_eq!(window.end(), None);
		assert_eq!(window.runs(3).collect::<Vec<_>>(), vec![3..=3, 7..=u64::MAX]);
		assert_eq!(window.clip(0..10), Some(2..4));
		assert_eq!(window.clip(4..10), Some(7..10));
		assert_eq!(window.clip(4..7), None);
		assert_eq!(
			window.resolved(),
			vec![
				Some(Resolved { start: 2, end: Some(4) }),
				Some(Resolved { start: 7, end: None }),
			]
		);

		// Nothing asked for is nothing in range, floored nowhere.
		let window = Window::new(&[], None, &Untimed).unwrap();
		assert_eq!(covered(&window, 4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// Relative anchors count back from the latest group, both ends whole groups, and wait
	/// for a first group when there is none.
	#[test]
	fn relative_anchors_count_back_from_the_latest_group() {
		let window = Window::new(&[range(Anchor::Relative(3), Anchor::Relative(1))], Some(7), &Untimed).unwrap();
		assert_eq!(covered(&window, 10), vec![4, 5, 6]);

		let mut window = Window::new(&[Range::live()], None, &Untimed).unwrap();
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None, &Untimed));
		assert!(window.settle(Some(5), &Untimed));
		assert_eq!(covered(&window, 8), vec![5, 6, 7]);
		assert!(window.is_settled());

		// The farthest offset needs no latest group, and names no floor.
		let window = Window::new(&[Range::oldest()], None, &Untimed).unwrap();
		assert_eq!((window.floor(), window.start()), (Some(0), None));
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let start = |ranges: &[Range], latest| Window::new(ranges, latest, &Untimed).unwrap().start();
		assert_eq!(start(&[Range::groups(4..), Range::live()], Some(9)), Some(4));
		assert_eq!(start(&[Range::groups(4..), Range::live()], Some(2)), Some(2));
		assert_eq!(start(&[Range::groups(0..)], None), Some(0));
		assert_eq!(start(&[Range::groups(4..), Range::live()], None), None);
		assert_eq!(start(&[Range::groups(4..), Range::oldest()], Some(9)), None);
		assert_eq!(start(&[Range::groups(4..), Range::groups(..2)], Some(9)), None);
	}

	/// A time start is the group containing it, and an end stops just past that group once
	/// a later group is known to start after it with nothing between left to arrive.
	#[test]
	fn time_anchors_resolve_from_group_starts() {
		let groups = Groups::new([
			Groups::started(2, 2000),
			Groups::started(3, 3000),
			Groups::started(4, 4000),
			Groups::started(5, 5000),
			(6, Held::Pending),
		]);
		let window = Window::new(&[range(time(3500), time(4500))], Some(6), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![3, 4]);
		assert!(window.is_settled());
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 3, end: Some(5) })]);

		// Group 6 may yet start at 5000 too: read on speculation, the end left open.
		let window = Window::new(&[range(time(3500), time(5000))], Some(6), &groups).unwrap();
		assert!(!window.is_settled());
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6]);
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 3, end: None })]);
		assert_eq!(window.end(), None, "the cap is not the end");
		assert_eq!(window.speculative().collect::<Vec<_>>(), vec![6]);

		// Before every start held: reach back to the oldest.
		let window = Window::new(&[range(time(1000), time(2500))], Some(6), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![0, 1, 2]);

		// Nothing started yet: a time start waits like the live edge.
		let mut window = Window::new(&[range(time(1000), None)], None, &Groups::default()).unwrap();
		assert!(window.is_pending());
		assert!(window.settle(Some(4), &Groups::new([(4, Held::Pending)])));
		assert_eq!(window.floor(), Some(4));
	}

	/// An end settles only once every sequence between the newest group starting inside it
	/// and the first starting past it is accounted for.
	#[test]
	fn a_time_end_settles_once_nothing_below_its_end_can_arrive() {
		let mut groups = Groups::new([Groups::started(3, 3000), Groups::started(6, 6000)]);
		let mut window = Window::new(&[range(time(3000), time(4000))], Some(6), &groups).unwrap();
		assert!(!window.is_settled(), "4 and 5 may still arrive inside the end");
		assert_eq!(
			covered(&window, 8),
			vec![3, 4],
			"only 4, the first that may still arrive, on speculation, and never 6"
		);

		// 4 arrives yet to start: still the one read on speculation, so 5 waits.
		groups.held.insert(4, Held::Pending);
		assert!(!window.settle(Some(6), &groups), "4 was already the speculative one");
		assert_eq!(covered(&window, 8), vec![3, 4]);
		groups.held.insert(4, Held::Started(at(3500)));
		assert!(window.settle(Some(6), &groups));
		assert!(!window.is_settled(), "5 may still arrive");
		groups.gone.push(5..6);
		assert!(window.settle(Some(6), &groups));
		assert!(window.is_settled());
		assert_eq!(covered(&window, 8), vec![3, 4, 5]);

		// A group that never starts proves nothing, so it never holds the end open.
		let groups = Groups::new([Groups::started(3, 3000), (4, Held::Never), Groups::started(5, 5000)]);
		let window = Window::new(&[range(time(3000), time(4000))], Some(5), &groups).unwrap();
		assert!(window.is_settled());
		assert_eq!(covered(&window, 8), vec![3, 4]);
	}

	/// An unsettled time end reads at most one group on speculation: the first above the
	/// newest known to start inside it that may still start. A group that never starts
	/// does not take that place, and a stalled one does not hold back a later group known
	/// to start inside the end.
	#[test]
	fn an_unsettled_time_end_reads_one_group_on_speculation() {
		let mut groups = Groups::new([(3, Held::Pending), (4, Held::Pending), (5, Held::Pending)]);
		let mut window = Window::new(&[range(Anchor::Absolute(3), time(3500))], Some(5), &groups).unwrap();
		assert_eq!(covered(&window, 8), vec![3]);

		groups.held.insert(3, Held::Started(at(3000)));
		assert!(window.settle(Some(5), &groups));
		assert_eq!(covered(&window, 8), vec![3, 4]);

		// 4 never starts, so 5 is the one read on speculation.
		groups.held.insert(4, Held::Never);
		assert!(window.settle(Some(5), &groups));
		assert_eq!(covered(&window, 8), vec![3, 4, 5]);

		// 5 stalls, 6 starts inside the end: 5 is in range then, and 7 is speculative.
		groups.held.insert(6, Held::Started(at(3400)));
		groups.held.insert(7, Held::Pending);
		assert!(window.settle(Some(7), &groups));
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6, 7]);

		// 7 starts past the end, which settles with 5 still stalled below it.
		groups.held.insert(7, Held::Started(at(3600)));
		assert!(window.settle(Some(7), &groups));
		assert!(window.is_settled());
		assert_eq!(covered(&window, 9), vec![3, 4, 5, 6]);

		// Every held group accounted for: the next to arrive is the speculative one.
		let groups = Groups::new([Groups::started(3, 3000)]);
		let window = Window::new(&[range(time(3000), time(3500))], Some(3), &groups).unwrap();
		assert!(window.contains(4));

		// Nothing more can arrive or start: uncapped.
		let mut groups = Groups::new([Groups::started(3, 3000), (4, Held::Pending), (5, Held::Pending)]);
		groups.complete = true;
		let window = Window::new(&[range(time(3000), time(3500))], Some(5), &groups).unwrap();
		assert_eq!(covered(&window, 7), vec![3, 4, 5, 6]);
	}

	/// A peer that leaves an early sequence missing and keeps sending later groups costs a
	/// time end each group once, not a rescan of everything since the gap on every settle.
	#[test]
	fn a_time_end_walks_each_group_once_behind_a_missing_sequence() {
		let mut groups = Groups::new([Groups::started(0, 0)]);
		let mut window = Window::new(&[range(time(0), time(1_000_000))], Some(0), &groups).unwrap();
		let count = 2_000;
		for sequence in 2..count {
			groups.held.insert(sequence, Held::Started(at(sequence * 10)));
			window.settle(Some(sequence), &groups);
		}
		assert!(!window.is_settled(), "group 1 never arrived");
		assert!(window.contains(count - 1));
		let asked = groups.asked.get();
		assert!(asked < 8 * count as usize, "{asked} queries for {count} groups");
	}

	/// A datagram is one frame, so its own timestamp places it against a time end.
	#[test]
	fn a_datagram_is_judged_by_its_own_timestamp() {
		let window = Window::new(&[range(Anchor::Absolute(0), time(3500))], None, &Groups::default()).unwrap();
		assert!(window.admits_datagram(9, Some(at(3500))));
		assert!(!window.admits_datagram(9, Some(at(3600))));
	}

	/// Groups whose `racing` one starts past the end after the first time it is asked
	/// about, as when its first frame lands between the walks of one settle.
	struct Racing {
		groups: Groups,
		racing: u64,
		asked: Cell<bool>,
	}

	impl Holdings for Racing {
		fn timed(&self) -> bool {
			true
		}

		fn containing(&self, pts: Timestamp) -> Containing {
			self.groups.containing(pts)
		}

		fn next_start(&self, from: u64) -> Option<(u64, Timestamp)> {
			self.groups.next_start(from)
		}

		fn next_held(&self, from: u64) -> Option<(u64, Held)> {
			let next = self.groups.next_held(from);
			if next.is_some_and(|(sequence, _)| sequence == self.racing) && self.asked.replace(true) {
				return Some((self.racing, Held::Started(at(6000))));
			}
			next
		}

		fn gone(&self, sequence: u64) -> Option<u64> {
			self.groups.gone(sequence)
		}

		fn complete(&self) -> bool {
			false
		}
	}

	#[test]
	fn a_time_end_settles_on_a_group_that_starts_between_its_walks() {
		let racing = Racing {
			groups: Groups::new([Groups::started(0, 0), (1, Held::Pending), Groups::started(2, 7000)]),
			racing: 1,
			asked: Cell::new(false),
		};
		let window = Window::new(&[range(Anchor::Absolute(0), time(5000))], None, &racing).unwrap();
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 0, end: Some(1) })]);
	}

	#[test]
	fn a_missing_sequence_past_a_time_end_is_the_one_read_on_speculation() {
		// 1 and 2 are missing, and 3 never starts: the walk may not pass the gap, or every
		// sequence in it that arrives later is read past the end.
		let mut groups = Groups::new([Groups::started(0, 3000), (3, Held::Never)]);
		let mut window = Window::new(&[range(time(3000), time(3500))], Some(3), &groups).unwrap();
		assert_eq!(covered(&window, 5), vec![0, 1]);
		assert_eq!(window.speculative().collect::<Vec<_>>(), vec![1]);
		groups.held.insert(2, Held::Pending);
		groups.held.insert(1, Held::Pending);
		window.settle(Some(3), &groups);
		assert_eq!(covered(&window, 5), vec![0, 1], "2 waits on 1 starting");
		groups.held.insert(1, Held::Started(at(3600)));
		window.settle(Some(3), &groups);
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 0, end: Some(1) })]);
	}

	#[test]
	fn a_time_end_stops_walking_at_the_last_sequence() {
		let groups = Groups::new([Groups::started(u64::MAX, 3000)]);
		let window = Window::new(&[range(Anchor::Absolute(u64::MAX), time(5000))], None, &groups).unwrap();
		assert_eq!(
			window.resolved(),
			vec![Some(Resolved {
				start: u64::MAX,
				end: None
			})]
		);
		assert!(window.contains(u64::MAX));
		assert_eq!(window.watch_starts(), None, "nothing above the last sequence can start");
	}

	#[test]
	fn a_settled_window_still_judges_a_datagram_by_its_times() {
		// 1 is a datagram the starts index never sees: the end settles before 2 by
		// sequence, but 1 presents past it.
		let groups = Groups {
			gone: std::iter::once(1..2).collect(),
			..Groups::new([Groups::started(0, 0), Groups::started(2, 10_000)])
		};
		let window = Window::new(&[range(time(0), time(5000))], None, &groups).unwrap();
		assert_eq!(window.resolved(), vec![Some(Resolved { start: 0, end: Some(2) })]);
		assert!(window.admits_datagram(1, Some(at(4000))));
		assert!(!window.admits_datagram(1, Some(at(7000))));
	}

	#[test]
	fn a_waiting_window_judges_a_datagram_against_both_times() {
		let window = Window::new(&[range(time(3000), time(3500))], None, &Groups::default()).unwrap();
		assert!(window.is_pending(), "no group has started");
		assert!(!window.admits_datagram(0, Some(at(2900))));
		assert!(window.admits_datagram(1, Some(at(3000))));
		assert!(window.admits_datagram(2, Some(at(3500))));
		assert!(!window.admits_datagram(3, Some(at(3600))));
	}

	#[test]
	fn a_waiting_window_judges_a_datagram_against_absolute_groups_too() {
		let window = Window::new(
			&[range(time(3000), Some(Anchor::Absolute(10)))],
			None,
			&Groups::default(),
		)
		.unwrap();
		assert!(window.is_pending(), "no group has started");
		assert!(window.admits_datagram(9, Some(at(3500))));
		assert!(!window.admits_datagram(10, Some(at(3500))));
		assert!(!window.admits_datagram(9, Some(at(2900))));
	}

	/// A datagram is in range only when one range admits its sequence and time together,
	/// not when each passes a different range.
	#[test]
	fn a_datagram_is_judged_against_one_range_at_a_time() {
		let groups = Groups::new([Groups::started(5, 5000)]);
		// The time range covers group 5 whole, but only from 5500 on.
		let window = Window::new(
			&[range(Anchor::Absolute(6), None), range(time(5500), None)],
			Some(5),
			&groups,
		)
		.unwrap();
		assert!(!window.admits_datagram(5, Some(at(5000))));
		assert!(window.admits_datagram(5, Some(at(5500))));
		assert!(window.admits_datagram(6, Some(at(5000))));
	}

	#[test]
	fn an_untimed_track_refuses_time_anchors() {
		assert!(matches!(
			Window::new(&[range(time(0), None)], None, &Untimed),
			Err(Error::TimestampMismatch)
		));
		assert!(matches!(
			Window::new(&[range(Anchor::Relative(0), time(0))], None, &Untimed),
			Err(Error::TimestampMismatch)
		));
	}
}
