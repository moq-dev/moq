//! A subscriber's own ranges, resolved to the group sequences it reads.
//!
//! The aggregate a publisher sees is the union of every subscriber's ranges, so it says
//! what arrives, not what one subscriber asked for. A [`Window`] is the per-subscriber
//! side: resolved at the hop serving the subscriber, it filters that subscriber's reads.

use std::ops;

use rangemap::RangeInclusiveSet;

use super::{Anchor, Range};

/// One range resolved to groups: from `start` up to the exclusive `end`, `None` while it
/// stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::model) struct Resolved {
	pub start: u64,
	pub end: Option<u64>,
}

/// A subscriber's [`Range`]s resolved to the group sequences it reads.
#[derive(Debug)]
pub(in crate::model) struct Window {
	/// What these were resolved from, so only replacing the ranges resolves again.
	ranges: Vec<Range>,
	spans: Vec<Span>,
	/// The groups the spans cover.
	groups: RangeInclusiveSet<u64>,
	/// A relative anchor waits on a first group, so nothing is filtered yet.
	pending: bool,
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
}

#[derive(Clone, Copy, Debug)]
enum End {
	/// Before this group.
	Before(u64),
	/// Past every group.
	Open,
}

impl Window {
	/// Resolve `ranges`, counting relative anchors back from `latest`.
	pub fn new(ranges: &[Range], latest: Option<u64>) -> Self {
		let mut window = Self {
			ranges: ranges.to_vec(),
			spans: Vec::new(),
			groups: RangeInclusiveSet::new(),
			pending: false,
		};
		window.place(latest);
		window
	}

	/// Whether a relative anchor still waits on a first group.
	pub fn is_pending(&self) -> bool {
		self.pending
	}

	/// Resolve the relative anchors waiting on a first group once `latest` exists.
	/// Returns whether anything moved.
	pub fn settle(&mut self, latest: Option<u64>) -> bool {
		if !self.pending || latest.is_none() {
			return false;
		}
		self.place(latest);
		!self.pending
	}

	/// Whether group `sequence` is in range. Everything is while a relative anchor waits.
	pub fn contains(&self, sequence: u64) -> bool {
		self.pending || self.groups.contains(&sequence)
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

	/// Each range resolved, in the order given: `None` for one that is empty or still
	/// waits on a first group.
	pub fn resolved(&self) -> Vec<Option<Resolved>> {
		let mut resolved = vec![None; self.ranges.len()];
		for span in &self.spans {
			resolved[span.index] = Some(Resolved {
				start: span.start,
				end: match span.end {
					End::Before(end) => Some(end),
					End::Open => None,
				},
			});
		}
		resolved
	}

	/// The exclusive end past every group in range, `None` when one stays open.
	pub fn end(&self) -> Option<u64> {
		if self.pending {
			return None;
		}
		match self.groups.iter().next_back() {
			Some(run) => run.end().checked_add(1),
			None => Some(0),
		}
	}

	/// The groups in range from `from` on, lowest first.
	pub fn runs(&self, from: u64) -> impl Iterator<Item = ops::RangeInclusive<u64>> + '_ {
		let all = self.pending.then_some(from..=u64::MAX);
		let runs = (!self.pending).then(|| {
			self.groups
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

	fn place(&mut self, latest: Option<u64>) {
		self.spans.clear();
		self.pending = false;
		for (index, range) in self.ranges.iter().enumerate().filter(|(_, range)| !range.is_empty()) {
			let (Some((start, floored)), Some(end)) = (start(range.start, latest), end(range.end, latest)) else {
				self.pending = true;
				continue;
			};
			self.spans.push(Span {
				index,
				start,
				end,
				floored,
			});
		}
		self.groups = RangeInclusiveSet::new();
		for span in &self.spans {
			let last = match span.end {
				End::Before(end) => match end.checked_sub(1) {
					Some(last) if last >= span.start => last,
					_ => continue,
				},
				End::Open => u64::MAX,
			};
			self.groups.insert(span.start..=last);
		}
	}
}

/// The first group a range starts at, and whether it names that group rather than
/// reaching back to the oldest one. `None` while it waits on a latest group.
fn start(anchor: Anchor, latest: Option<u64>) -> Option<(u64, bool)> {
	match anchor {
		Anchor::Absolute(start) => Some((start.group, true)),
		// Nothing sits below group 0, so the farthest offset needs no latest group.
		Anchor::Relative(u64::MAX) => Some((0, false)),
		Anchor::Relative(back) => Some((latest?.saturating_sub(back), true)),
	}
}

/// Where a range stops, or `None` while it waits on a latest group.
fn end(anchor: Option<Anchor>, latest: Option<u64>) -> Option<End> {
	Some(match anchor {
		None => End::Open,
		Some(Anchor::Absolute(end)) => end.final_sequence().map_or(End::Open, End::Before),
		Some(Anchor::Relative(back)) => latest?
			.saturating_sub(back)
			.checked_add(1)
			.map_or(End::Open, End::Before),
	})
}

#[cfg(test)]
mod tests {
	use super::super::Position;
	use super::*;

	fn range(start: Anchor, end: impl Into<Option<Anchor>>) -> Range {
		Range { start, end: end.into() }
	}

	fn covered(window: &Window, upto: u64) -> Vec<u64> {
		(0..upto).filter(|sequence| window.contains(*sequence)).collect()
	}

	/// Disjoint ranges filter to exactly their groups; the gap between them is out.
	#[test]
	fn ranges_cover_only_their_groups() {
		let window = Window::new(&[Range::groups(2..4), Range::groups(7..)], Some(9));
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

		// A mid-group end includes its group, a head-of-group end excludes it.
		let window = Window::new(
			&[range(
				Anchor::Absolute(Position { group: 1, frame: 2 }),
				Anchor::Absolute(Position { group: 3, frame: 1 }),
			)],
			None,
		);
		assert_eq!(covered(&window, 6), vec![1, 2, 3]);
		assert_eq!(window.end(), Some(4));

		// Nothing asked for is nothing in range, floored nowhere.
		let window = Window::new(&[], None);
		assert_eq!(covered(&window, 4), Vec::<u64>::new());
		assert_eq!((window.floor(), window.end()), (Some(0), Some(0)));
	}

	/// Relative anchors count back from the latest group, both ends whole groups, and wait
	/// for a first group when there is none.
	#[test]
	fn relative_anchors_count_back_from_the_latest_group() {
		let window = Window::new(&[range(Anchor::Relative(3), Anchor::Relative(1))], Some(7));
		assert_eq!(covered(&window, 10), vec![4, 5, 6]);

		let mut window = Window::new(&[Range::live()], None);
		assert!(window.is_pending() && window.contains(3) && window.floor().is_none());
		assert!(!window.settle(None));
		assert!(window.settle(Some(5)));
		assert_eq!(covered(&window, 8), vec![5, 6, 7]);
		assert!(!window.is_pending());

		// The farthest offset needs no latest group, and names no floor.
		let window = Window::new(&[Range::oldest()], None);
		assert_eq!((window.floor(), window.start()), (Some(0), None));
	}

	/// The named floor is the lowest start, unless a range reaches back to the oldest
	/// group or still waits on a first one; an explicit group 0 names one.
	#[test]
	fn the_named_start_is_the_lowest_unless_one_reaches_back() {
		let start = |ranges: &[Range], latest| Window::new(ranges, latest).start();
		assert_eq!(start(&[Range::groups(4..), Range::live()], Some(9)), Some(4));
		assert_eq!(start(&[Range::groups(4..), Range::live()], Some(2)), Some(2));
		assert_eq!(start(&[Range::groups(0..)], None), Some(0));
		assert_eq!(start(&[Range::groups(4..), Range::live()], None), None);
		assert_eq!(start(&[Range::groups(4..), Range::oldest()], Some(9)), None);
		assert_eq!(start(&[Range::groups(4..), Range::groups(..2)], Some(9)), None);
	}
}
