//! The union of every subscriber's ranges, kept current per change rather than refolded.
//!
//! Time ranges become signed boundaries over group starts, +1 where one starts and -1
//! past where it ends, so a start is covered while the running sum stays above zero. A
//! start between groups stands for the group containing it, which a range starting at or
//! before it covers too, so spans merge as they do over starts. A treap keeps each
//! subtree's sum and lowest running sum, so a change touches two boundaries and asks one
//! range-minimum question, whatever else the audience holds. Ranges with no time start can't
//! be placed on that line and are counted by value instead, live joins by lag and end under
//! the largest `max` among them, since where a join lands doesn't depend on it.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};

use std::time::Duration;

use super::super::{Range, Start};
use super::{add, remove};
use crate::model::subscription::max_of;

/// Every subscriber's ranges, counted so that one change costs O(log n).
#[derive(Default)]
pub(super) struct Ranges {
	/// Holders of a range from the oldest group following the live edge, which covers every
	/// other range.
	open: usize,
	/// Ranges with a time start, as spans of starts.
	time: Coverage,
	/// Ranges from the oldest group with an end, kept as they are.
	others: BTreeMap<Range, usize>,
	/// Live joins by lag and end, each with the budgets its holders ask under.
	joins: BTreeMap<(Duration, Option<u64>), BTreeMap<Duration, usize>>,
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
	Join((Duration, Option<u64>), Duration),
	Other,
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
			(Start::Oldest, Some(_)) => Self::Other,
		}
	}
}

impl Ranges {
	/// Count `range`, returning whether the union changed.
	pub(super) fn add(&mut self, range: Range) -> bool {
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
			Shape::Other => add(&mut self.others, range) && !hidden,
		};
		self.union.take();
		changed
	}

	/// Uncount `range`, returning whether the union changed.
	pub(super) fn remove(&mut self, range: Range) -> bool {
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
			Shape::Other => remove(&mut self.others, range) && !hidden(self),
		};
		self.union.take();
		changed
	}

	/// The union in [`Range`] order, costing its own length rather than the audience's.
	pub(super) fn union(&self) -> Vec<Range> {
		self.union.get_or_init(|| self.fold()).clone()
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
			})
			.collect();
		union.extend(self.others.keys());
		union.extend(self.joins.iter().filter_map(|(&(min, end), budgets)| {
			let budget = *budgets.last_key_value()?.0;
			Some(Range {
				start: Start::Live {
					min,
					max: max_of(budget),
				},
				end,
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

	/// Each change reports exactly whether the union moved, and the union matches a full fold
	/// of what is held.
	#[test]
	fn incremental_union_matches_a_full_fold() {
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x72616e676573);
		let start = |rng: &mut rand::rngs::StdRng| match rng.random_range(0..10) {
			0 => Start::Oldest,
			1 => Start::Live {
				min: Duration::from_millis(rng.random_range(0..3)),
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
