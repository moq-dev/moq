//! The union of every subscriber's ranges, kept current per change rather than refolded.
//!
//! Ranges are counted apart by [`Policy`], since folding one into a range at another policy
//! would lose its priority or order. Within one policy, time ranges become signed boundaries
//! over group starts, +1 where one starts and -1 past where it ends, so a start is covered
//! while the running sum stays above zero. A start between groups stands for the group
//! containing it, which a range starting at or before it covers too, so spans merge as they
//! do over starts. A treap keeps each subtree's sum and lowest running sum, so a change
//! touches two boundaries and asks one range-minimum question, whatever else the audience
//! holds. Ranges with no time start can't be placed on that line and are counted by value
//! instead: live joins by lag and end under the largest `max` among them, since where a join
//! lands doesn't depend on it, and ranges from the oldest group under the furthest end, which
//! takes in the rest.
//!
//! Across policies a range is dropped only once ranges at policies
//! [covering](Policy::covers) its own cover its span. With one policy there is nothing to
//! fold, so a change costs O(log n) as above. With several, the fold is redone only when one
//! policy's union moves, at the cost of the union's own length.

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};

use std::time::Duration;

use super::super::{Order, Range, Start};
use super::{add, remove};
use crate::model::subscription::{Policy, max_of};

/// Every subscriber's ranges, counted so that one change costs O(log n) unless several
/// policies are asked and it moves the union of its policy's ranges.
#[derive(Default)]
pub(super) struct Ranges {
	/// The ranges of each policy, those covering others first.
	classes: BTreeMap<(Reverse<i8>, Reverse<Order>), (Policy, Class)>,
	/// The union, until a change moves it. Kept while several policies are asked, to tell
	/// whether a change moved it.
	union: OnceCell<Vec<Range>>,
}

impl Ranges {
	/// Count `range`, returning whether the union changed.
	pub(super) fn add(&mut self, range: Range) -> bool {
		if range.is_empty() {
			return false;
		}
		let policy = range.policy();
		let several = self.classes.len() > 1 || self.classes.keys().any(|rank| *rank != policy.rank());
		self.held(several);
		let (_, class) = (self.classes.entry(policy.rank())).or_insert_with(|| (policy, Class::default()));
		class.add(range) && self.moved(several)
	}

	/// Uncount `range`, returning whether the union changed.
	pub(super) fn remove(&mut self, range: Range) -> bool {
		if range.is_empty() {
			return false;
		}
		let several = self.classes.len() > 1;
		self.held(several);
		let rank = range.policy().rank();
		let (_, class) = self.classes.get_mut(&rank).expect("a counted policy");
		let moved = class.remove(range);
		if class.held == 0 {
			self.classes.remove(&rank);
		}
		moved && self.moved(several)
	}

	/// The union in [`Range`] order, costing its own length rather than the audience's.
	pub(super) fn union(&self) -> Vec<Range> {
		self.union.get_or_init(|| self.fold()).clone()
	}

	/// Hold the union before a change when `several` policies are asked around it, so
	/// [`Self::moved`] has it to compare against.
	fn held(&self, several: bool) {
		if several {
			self.union.get_or_init(|| self.fold());
		}
	}

	/// Whether a change that moved one policy's union moved the union: always with one policy
	/// asked, and otherwise only when the fold comes out different.
	fn moved(&mut self, several: bool) -> bool {
		let Some(old) = self.union.take() else {
			return true;
		};
		if !several {
			return true;
		}
		let union = self.fold();
		let moved = union != old;
		self.union = OnceCell::from(union);
		moved
	}

	/// Fold every policy's union, dropping a range that ranges at covering policies cover.
	fn fold(&self) -> Vec<Range> {
		let mut above = Above::default();
		let mut union = Vec::new();
		for (policy, class) in self.classes.values() {
			let spans = class.union();
			union.extend(
				(spans.iter())
					.filter(|range| !above.covers(range))
					.map(|range| range.with_policy(*policy)),
			);
			above.extend(spans);
		}
		union.sort_unstable();
		union
	}
}

/// What ranges at covering policies already ask, as a range of one policy is checked against
/// them: the same spans [`Class`] merges ranges into.
#[derive(Default)]
struct Above {
	/// Whether a range from the oldest group follows the live edge, covering everything.
	all: bool,
	/// The time spans, merged where they overlap or touch, as exclusive spans of starts by start.
	runs: BTreeMap<u64, Option<u64>>,
	/// The furthest end of a range from the oldest group.
	oldest: Option<u64>,
	/// Live joins by lag and end, under the widest budget.
	joins: BTreeMap<(Option<Duration>, Option<u64>), Duration>,
}

impl Above {
	fn covers(&self, range: &Range) -> bool {
		if self.all {
			return true;
		}
		match Shape::of(range) {
			Shape::Empty => true,
			Shape::Open => false,
			Shape::Time(start, end) => self.runs.range(..=start).next_back().is_some_and(|(_, run)| match run {
				None => true,
				Some(run) => end.is_some_and(|end| end <= *run),
			}),
			Shape::Join(join, budget) => self.joins.get(&join).is_some_and(|widest| *widest >= budget),
			Shape::Oldest(end) => self.oldest.is_some_and(|furthest| furthest >= end),
		}
	}

	/// Take in one policy's union.
	fn extend(&mut self, ranges: &[Range]) {
		for range in ranges {
			match Shape::of(range) {
				Shape::Empty => {}
				Shape::Open => self.all = true,
				Shape::Time(start, end) => self.span(start, end),
				Shape::Join(join, budget) => {
					let widest = self.joins.entry(join).or_default();
					*widest = (*widest).max(budget);
				}
				Shape::Oldest(end) => self.oldest = self.oldest.max(Some(end)),
			}
		}
	}

	/// Merge the exclusive span of starts `start..end` into [`Self::runs`].
	fn span(&mut self, mut start: u64, mut end: Option<u64>) {
		// One before it that reaches it takes it in.
		if let Some((&before, &reach)) = self.runs.range(..=start).next_back()
			&& reach.is_none_or(|reach| reach >= start)
		{
			start = before;
			end = end.zip(reach).map(|(end, reach)| end.max(reach));
		}
		// As does it each one starting inside it.
		while let Some((&next, &reach)) = self.runs.range(start..).next() {
			if end.is_some_and(|end| next > end) {
				break;
			}
			self.runs.remove(&next);
			end = end.zip(reach).map(|(end, reach)| end.max(reach));
		}
		self.runs.insert(start, end);
	}
}

/// The ranges of one policy, counted so that one change costs O(log n).
#[derive(Default)]
struct Class {
	/// How many non-empty ranges are counted.
	held: usize,
	/// Holders of a range from the oldest group following the live edge, which covers every
	/// other range.
	open: usize,
	/// Ranges with a time start, as spans of starts.
	time: Coverage,
	/// Ranges from the oldest group with an end, by end: the furthest takes in the rest.
	oldest: BTreeMap<u64, usize>,
	/// Live joins by lag and end, each with the budgets its holders ask under.
	joins: BTreeMap<(Option<Duration>, Option<u64>), BTreeMap<Duration, usize>>,
	/// The last union read, until a change moves it.
	union: OnceCell<Vec<Range>>,
}

/// How [`Ranges`] counts one range.
enum Shape {
	Empty,
	Open,
	/// A range with a time start, as the exclusive span of starts it covers.
	Time(u64, Option<u64>),
	/// A live join, by lag and end, and its budget.
	Join((Option<Duration>, Option<u64>), Duration),
	/// A range from the oldest group, by its end.
	Oldest(u64),
}

impl Shape {
	fn of(range: &Range) -> Self {
		match (range.start, range.end) {
			_ if range.is_empty() => Self::Empty,
			(Start::Oldest, None) => Self::Open,
			(Start::Time(start), None) => Self::Time(start, None),
			// An end at the last start covers every group, as an open one does.
			(Start::Time(start), Some(end)) => Self::Time(start, end.checked_add(1)),
			(Start::Live { min, .. }, end) => Self::Join((min, end), range.budget()),
			(Start::Oldest, Some(end)) => Self::Oldest(end),
		}
	}
}

impl Class {
	/// Count `range`, returning whether the union changed.
	fn add(&mut self, range: Range) -> bool {
		let hidden = self.open > 0;
		let changed = match Shape::of(&range) {
			Shape::Empty => return false,
			Shape::Open => {
				self.open += 1;
				self.open == 1
			}
			Shape::Time(start, end) => {
				let uncovered = self.time.min(start, end) == 0;
				self.time.add(start, end, 1);
				uncovered && !hidden
			}
			Shape::Join(join, budget) => {
				let budgets = self.joins.entry(join).or_default();
				let widest = budgets.last_key_value().map(|(widest, _)| *widest);
				add(budgets, budget);
				widest.is_none_or(|widest| budget > widest) && !hidden
			}
			Shape::Oldest(end) => {
				let furthest = self.oldest.last_key_value().map(|(furthest, _)| *furthest);
				add(&mut self.oldest, end);
				furthest.is_none_or(|furthest| end > furthest) && !hidden
			}
		};
		self.held += 1;
		if changed {
			self.union.take();
		}
		changed
	}

	/// Uncount `range`, returning whether the union changed.
	fn remove(&mut self, range: Range) -> bool {
		let hidden = |ranges: &Self| ranges.open > 0;
		let changed = match Shape::of(&range) {
			Shape::Empty => return false,
			Shape::Open => {
				self.open -= 1;
				self.open == 0
			}
			Shape::Time(start, end) => {
				self.time.add(start, end, -1);
				!hidden(self) && self.time.min(start, end) == 0
			}
			Shape::Join(join, budget) => {
				let budgets = self.joins.get_mut(&join).expect("registered join");
				let widest = budgets.last_key_value().map(|(widest, _)| *widest);
				remove(budgets, budget);
				let moved = budgets.last_key_value().map(|(widest, _)| *widest) != widest;
				if budgets.is_empty() {
					self.joins.remove(&join);
				}
				moved && !hidden(self)
			}
			Shape::Oldest(end) => {
				let furthest = self.oldest.last_key_value().map(|(furthest, _)| *furthest);
				remove(&mut self.oldest, end);
				self.oldest.last_key_value().map(|(furthest, _)| *furthest) != furthest && !hidden(self)
			}
		};
		self.held -= 1;
		if changed {
			self.union.take();
		}
		changed
	}

	/// The union at the default policy, in [`Range`] order, costing its own length rather
	/// than the audience's.
	fn union(&self) -> &[Range] {
		self.union.get_or_init(|| self.fold())
	}

	fn fold(&self) -> Vec<Range> {
		if self.open > 0 {
			return vec![Range::ALL];
		}
		let mut union: Vec<Range> = self
			.time
			.runs()
			.into_iter()
			.map(|(start, end)| Range {
				start: Start::Time(start),
				// A run past its start ends at or past one more.
				end: end.map(|end| end - 1),
				..Range::ALL
			})
			.collect();
		union.extend(self.oldest.last_key_value().map(|(&end, _)| Range {
			start: Start::Oldest,
			end: Some(end),
			..Range::ALL
		}));
		union.extend(self.joins.iter().filter_map(|(&(min, end), budgets)| {
			let budget = *budgets.last_key_value()?.0;
			Some(Range {
				start: Start::Live {
					min,
					max: max_of(budget),
				},
				end,
				..Range::ALL
			})
		}));
		union.sort_unstable();
		union
	}
}

/// Signed boundary counts over group starts: the coverage of the time ranges.
#[derive(Default)]
struct Coverage {
	root: Link,
	priorities: RandomState,
}

type Link = Option<Box<Node>>;

struct Node {
	key: u64,
	/// Ranges starting here, minus ranges ending here. Never zero.
	delta: i64,
	priority: u64,
	/// The sum of every delta in this subtree.
	sum: i64,
	/// The lowest running sum at any key of this subtree, counted from its first key.
	low: i64,
	left: Link,
	right: Link,
}

impl Coverage {
	/// Count `delta` more ranges over `start..end`, open when `end` is `None`.
	fn add(&mut self, start: u64, end: Option<u64>, delta: i64) {
		self.shift(start, delta);
		if let Some(end) = end {
			self.shift(end, -delta);
		}
	}

	fn shift(&mut self, key: u64, delta: i64) {
		let (below, rest) = split(self.root.take(), &key, false);
		let (at, above) = split(rest, &key, true);
		let delta = at.map_or(0, |node| node.delta) + delta;
		let at = (delta != 0).then(|| Node::new(key, delta, self.priorities.hash_one(key)));
		self.root = merge(merge(below, at), above);
	}

	/// The fewest ranges covering any group in `start..end`.
	fn min(&mut self, start: u64, end: Option<u64>) -> i64 {
		let (through, rest) = split(self.root.take(), &start, true);
		let (inside, above) = match end {
			Some(end) => split(rest, &end, false),
			None => (rest, None),
		};
		let at = sum(&through);
		let min = at.min(at.saturating_add(low(&inside)));
		self.root = merge(through, merge(inside, above));
		min
	}

	/// The covered spans in order, the last one open while an open range remains.
	fn runs(&self) -> Vec<(u64, Option<u64>)> {
		let mut runs = Vec::new();
		walk(&self.root, 0, &mut runs);
		runs
	}
}

impl Node {
	fn new(key: u64, delta: i64, priority: u64) -> Box<Self> {
		Box::new(Self {
			key,
			delta,
			priority,
			sum: delta,
			low: delta,
			left: None,
			right: None,
		})
	}

	fn update(&mut self) {
		let at = sum(&self.left) + self.delta;
		self.sum = at + sum(&self.right);
		self.low = low(&self.left).min(at).min(at.saturating_add(low(&self.right)));
	}
}

fn sum(link: &Link) -> i64 {
	link.as_ref().map_or(0, |node| node.sum)
}

fn low(link: &Link) -> i64 {
	link.as_ref().map_or(i64::MAX, |node| node.low)
}

/// Split into the keys below `key` (through it when `inclusive`) and the rest.
fn split(link: Link, key: &u64, inclusive: bool) -> (Link, Link) {
	let Some(mut node) = link else {
		return (None, None);
	};
	if node.key < *key || (inclusive && node.key == *key) {
		let (inner, rest) = split(node.right.take(), key, inclusive);
		node.right = inner;
		node.update();
		(Some(node), rest)
	} else {
		let (rest, inner) = split(node.left.take(), key, inclusive);
		node.left = inner;
		node.update();
		(rest, Some(node))
	}
}

/// Join two treaps, every key of `left` below every key of `right`.
fn merge(left: Link, right: Link) -> Link {
	match (left, right) {
		(None, link) | (link, None) => link,
		(Some(mut left), Some(mut right)) => {
			if left.priority > right.priority {
				left.right = merge(left.right.take(), Some(right));
				left.update();
				Some(left)
			} else {
				right.left = merge(Some(left), right.left.take());
				right.update();
				Some(right)
			}
		}
	}
}

/// Append the covered spans under `link`, entered with `offset` ranges open.
///
/// A subtree entered covered whose running sum never reaches zero holds no boundary of the
/// union, so it is skipped whole: the walk visits O(log n) nodes per span it returns.
fn walk(link: &Link, offset: i64, runs: &mut Vec<(u64, Option<u64>)>) {
	let Some(node) = link else {
		return;
	};
	if offset > 0 && offset.saturating_add(node.low) > 0 {
		return;
	}
	walk(&node.left, offset, runs);
	let before = offset + sum(&node.left);
	let at = before + node.delta;
	if before == 0 {
		runs.push((node.key, None));
	} else if at == 0 {
		runs.last_mut().expect("an open run").1 = Some(node.key);
	}
	walk(&node.right, at, runs);
}

#[cfg(test)]
mod tests {
	use super::super::super::super::subscription::union;
	use super::*;
	use rand::{RngExt, SeedableRng};

	/// A range from the oldest group with an end takes in every one ending before it, so only
	/// the furthest end moves the union.
	#[test]
	fn ranges_from_the_oldest_keep_only_the_furthest_end() {
		let oldest = |end| Range {
			start: Start::Oldest,
			end: Some(end),
			..Range::ALL
		};
		let mut ranges = Ranges::default();
		assert!(ranges.add(oldest(3)));
		assert!(ranges.add(oldest(5)), "a further end moves the union");
		assert!(!ranges.add(oldest(4)), "a nearer one does not");
		assert!(!ranges.add(oldest(5)));
		assert_eq!(ranges.union(), [oldest(5)]);
		assert!(!ranges.remove(oldest(5)), "another holds the furthest end");
		assert!(!ranges.remove(oldest(3)));
		assert!(ranges.remove(oldest(5)), "the furthest end left");
		assert_eq!(ranges.union(), [oldest(4)]);
	}

	/// Each change reports exactly whether the union moved, and the union matches a full fold
	/// of what is held, across a few policies that cover one another in each way.
	#[test]
	fn incremental_union_matches_a_full_fold() {
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x72616e676573);
		let start = |rng: &mut rand::rngs::StdRng| match rng.random_range(0..10) {
			0 => Start::Oldest,
			1 => Start::Live {
				// One in four with no lag limit.
				min: [None, Some(0), Some(1), Some(2)][rng.random_range(0..4)].map(Duration::from_millis),
				max: [None, Some(Duration::ZERO), Some(Duration::from_millis(5))][rng.random_range(0..3)],
			},
			_ => Start::Time(rng.random_range(0..12)),
		};
		let mut ranges = Ranges::default();
		let mut held: Vec<Range> = Vec::new();
		for step in 0..50_000 {
			let grow = if held.len() < 24 { 0.65 } else { 0.35 };
			let before = union(held.iter().copied());
			let changed = if held.is_empty() || rng.random_bool(grow) {
				let range = Range {
					start: start(&mut rng),
					end: rng.random_bool(0.6).then(|| rng.random_range(0..12)),
					priority: rng.random_range(0..3),
					order: match rng.random_bool(0.5) {
						true => Order::Ascending,
						false => Order::Descending,
					},
				};
				held.push(range);
				ranges.add(range)
			} else {
				let range = held.swap_remove(rng.random_range(0..held.len()));
				ranges.remove(range)
			};
			let after = union(held.iter().copied());
			assert_eq!(ranges.union(), after, "step {step}");
			assert_eq!(changed, before != after, "step {step}");
		}
	}
}
