//! The union of every subscriber's ranges, kept current per change rather than refolded.
//!
//! Time ranges become signed boundaries over group starts, +1 where one starts and -1
//! past where it ends, so a start is covered while the running sum stays above zero. A
//! start between groups stands for the group containing it, which a range starting at or
//! before it covers too, so spans merge as they do over starts. A treap keeps each
//! subtree's sum and lowest running sum, so a change touches two boundaries and asks one
//! range-minimum question, whatever else the audience holds. Ranges with no start can't be
//! placed on that line and are counted by value instead.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};

use super::super::Range;
use super::{add, remove};

/// Every subscriber's ranges, counted so that one change costs O(log n).
#[derive(Default)]
pub(super) struct Ranges {
	/// Holders of an open range with no start, which covers every other range while it
	/// reaches back as far as the budget allows.
	open: usize,
	/// Ranges with a start, as spans of starts.
	time: Coverage,
	/// Ranges with no start but an end, kept as they are.
	bounded: BTreeMap<Range, usize>,
	/// The last union read while the open range reaches back, and while it doesn't, until a
	/// change moves it.
	union: [OnceCell<Vec<Range>>; 2],
}

/// How [`Ranges`] counts one range.
enum Shape {
	Empty,
	Open,
	/// A range with a start, as the exclusive span of starts it covers.
	Time(u64, Option<u64>),
	Bounded,
}

impl Shape {
	fn of(range: &Range) -> Self {
		match (range.start, range.end) {
			_ if range.is_empty() => Self::Empty,
			(None, None) => Self::Open,
			(Some(start), None) => Self::Time(start, None),
			// An end at the last start covers every group, as an open one does.
			(Some(start), Some(end)) => Self::Time(start, end.checked_add(1)),
			(None, Some(_)) => Self::Bounded,
		}
	}
}

impl Ranges {
	/// Count `range`, returning whether the union changed, the open range reaching back as
	/// `reaches_back` says.
	pub(super) fn add(&mut self, range: Range, reaches_back: bool) -> bool {
		let hidden = self.open > 0 && reaches_back;
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
			Shape::Bounded => add(&mut self.bounded, range) && !hidden,
		};
		self.moved();
		changed
	}

	/// Uncount `range`, returning whether the union changed, the open range reaching back as
	/// `reaches_back` says.
	pub(super) fn remove(&mut self, range: Range, reaches_back: bool) -> bool {
		let hidden = |ranges: &Self| ranges.open > 0 && reaches_back;
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
			Shape::Bounded => remove(&mut self.bounded, range) && !hidden(self),
		};
		self.moved();
		changed
	}

	/// Whether an open range with no start is held, which hides every other range while it
	/// reaches back.
	pub(super) fn has_open(&self) -> bool {
		self.open > 0
	}

	/// Drop the cached unions: whichever one the next read takes may have moved.
	fn moved(&mut self) {
		for union in &mut self.union {
			union.take();
		}
	}

	/// The union in [`Range`] order, costing its own length rather than the audience's. An
	/// open range with no start hides the rest only when `reaches_back`.
	pub(super) fn union(&self, reaches_back: bool) -> Vec<Range> {
		self.union[usize::from(reaches_back)]
			.get_or_init(|| self.fold(reaches_back))
			.clone()
	}

	fn fold(&self, reaches_back: bool) -> Vec<Range> {
		if self.open > 0 && reaches_back {
			return vec![Range::default()];
		}
		let mut union: Vec<Range> = self
			.time
			.runs()
			.into_iter()
			.map(|(start, end)| Range {
				start: Some(start),
				// A run past its start ends at or past one more.
				end: end.map(|end| end - 1),
			})
			.collect();
		union.extend(self.bounded.keys());
		if self.open > 0 {
			union.push(Range::default());
		}
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
	/// of what is held, whether or not the open range reaches back.
	#[test]
	fn incremental_union_matches_a_full_fold() {
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x72616e676573);
		let time = |rng: &mut rand::rngs::StdRng| rng.random_bool(0.8).then(|| rng.random_range(0..12));
		let mut ranges = Ranges::default();
		let mut held: Vec<Range> = Vec::new();
		for step in 0..50_000 {
			let reaches_back = step % 1000 < 500;
			let grow = if held.len() < 24 { 0.65 } else { 0.35 };
			let before = union(held.iter().copied(), reaches_back);
			let changed = if held.is_empty() || rng.random_bool(grow) {
				let range = Range {
					start: time(&mut rng),
					end: rng.random_bool(0.6).then(|| rng.random_range(0..12)),
				};
				held.push(range);
				ranges.add(range, reaches_back)
			} else {
				let range = held.swap_remove(rng.random_range(0..held.len()));
				ranges.remove(range, reaches_back)
			};
			let after = union(held.iter().copied(), reaches_back);
			assert_eq!(ranges.union(reaches_back), after, "step {step}");
			assert_eq!(changed, before != after, "step {step}");
			assert_eq!(
				ranges.union(!reaches_back),
				union(held.iter().copied(), !reaches_back),
				"step {step}"
			);
		}
	}
}
