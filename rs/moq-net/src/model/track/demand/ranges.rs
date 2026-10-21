//! The union of every subscriber's ranges, kept current per change rather than refolded.
//!
//! Absolute ranges become signed boundaries over positions, +1 where one starts and -1
//! where it ends, so a position is covered while the running sum stays above zero. A treap
//! keeps each subtree's sum and lowest running sum, so a change touches two boundaries and
//! asks one range-minimum question, whatever else the audience holds. Ranges anchored to
//! the latest group or to a time can't be placed on that line and are counted by value
//! instead.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};

use super::super::{Anchor, Position, Range};
use super::{add, remove};

/// Every subscriber's ranges, counted so that one change costs O(log n).
#[derive(Default)]
pub(super) struct Ranges {
	/// Holders of [`Range::oldest`], which covers every other range.
	oldest: usize,
	absolute: Coverage,
	/// Open relative ranges by how far back they start: only the farthest matters.
	relative: BTreeMap<u64, usize>,
	/// Ranges with one relative end, or a bounded relative span, kept as they are.
	mixed: BTreeMap<Range, usize>,
	/// The last union read, until a change moves it.
	union: OnceCell<Vec<Range>>,
}

/// How [`Ranges`] counts one range.
enum Shape {
	Empty,
	Oldest,
	Absolute(Position, Option<Position>),
	Relative(u64),
	Mixed,
}

impl Shape {
	fn of(range: &Range) -> Self {
		match (range.start, range.end) {
			_ if range.is_empty() => Self::Empty,
			(Anchor::Relative(u64::MAX), None) => Self::Oldest,
			(Anchor::Absolute(start), None) => Self::Absolute(start, None),
			(Anchor::Absolute(start), Some(Anchor::Absolute(end))) => Self::Absolute(start, Some(end)),
			(Anchor::Relative(back), None) => Self::Relative(back),
			_ => Self::Mixed,
		}
	}
}

impl Ranges {
	/// Count `range`, returning whether the union changed.
	pub(super) fn add(&mut self, range: Range) -> bool {
		let changed = match Shape::of(&range) {
			Shape::Empty => false,
			Shape::Oldest => {
				self.oldest += 1;
				self.oldest == 1
			}
			Shape::Absolute(start, end) => {
				let uncovered = self.absolute.min(start, end) == 0;
				self.absolute.add(start, end, 1);
				uncovered && self.oldest == 0
			}
			Shape::Relative(back) => {
				let farthest = self.farthest();
				add(&mut self.relative, back);
				self.oldest == 0 && self.farthest() != farthest
			}
			Shape::Mixed => add(&mut self.mixed, range) && self.oldest == 0 && !self.covered(&range),
		};
		self.moved(changed)
	}

	/// Uncount `range`, returning whether the union changed.
	pub(super) fn remove(&mut self, range: Range) -> bool {
		let changed = match Shape::of(&range) {
			Shape::Empty => false,
			Shape::Oldest => {
				self.oldest -= 1;
				self.oldest == 0
			}
			Shape::Absolute(start, end) => {
				self.absolute.add(start, end, -1);
				self.oldest == 0 && self.absolute.min(start, end) == 0
			}
			Shape::Relative(back) => {
				let farthest = self.farthest();
				remove(&mut self.relative, back);
				self.oldest == 0 && self.farthest() != farthest
			}
			Shape::Mixed => remove(&mut self.mixed, range) && self.oldest == 0 && !self.covered(&range),
		};
		self.moved(changed)
	}

	/// Drop the cached union when `changed`, passing it through.
	fn moved(&mut self, changed: bool) -> bool {
		if changed {
			self.union.take();
		}
		changed
	}

	/// The union in [`Range`] order, costing its own length rather than the audience's.
	pub(super) fn union(&self) -> Vec<Range> {
		self.union.get_or_init(|| self.fold()).clone()
	}

	fn fold(&self) -> Vec<Range> {
		if self.oldest > 0 {
			return vec![Range::oldest()];
		}
		let mut union: Vec<Range> = self
			.absolute
			.runs()
			.into_iter()
			.map(|(start, end)| Range {
				start: Anchor::Absolute(start),
				end: end.map(Anchor::Absolute),
			})
			.collect();
		union.extend(self.mixed.keys().filter(|range| !self.covered(range)));
		union.extend(self.farthest().map(|back| Range {
			start: Anchor::Relative(back),
			end: None,
		}));
		union.sort_unstable();
		union
	}

	fn farthest(&self) -> Option<u64> {
		self.relative.last_key_value().map(|(back, _)| *back)
	}

	/// Whether an open range already delivers everything a mixed `range` could.
	fn covered(&self, range: &Range) -> bool {
		match range.start {
			Anchor::Absolute(start) => self.absolute.tail().is_some_and(|tail| tail <= start),
			Anchor::Relative(back) => self.farthest().is_some_and(|farthest| back <= farthest),
			// Where a time falls among the groups is the serving hop's to say.
			Anchor::Time(_) => false,
		}
	}
}

/// Signed boundary counts over positions: the coverage of the absolute ranges.
#[derive(Default)]
struct Coverage {
	root: Link,
	priorities: RandomState,
}

type Link = Option<Box<Node>>;

struct Node {
	key: Position,
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
	fn add(&mut self, start: Position, end: Option<Position>, delta: i64) {
		self.shift(start, delta);
		if let Some(end) = end {
			self.shift(end, -delta);
		}
	}

	fn shift(&mut self, key: Position, delta: i64) {
		let (below, rest) = split(self.root.take(), &key, false);
		let (at, above) = split(rest, &key, true);
		let delta = at.map_or(0, |node| node.delta) + delta;
		let at = (delta != 0).then(|| Node::new(key, delta, self.priorities.hash_one(key)));
		self.root = merge(merge(below, at), above);
	}

	/// The fewest ranges covering any position in `start..end`.
	fn min(&mut self, start: Position, end: Option<Position>) -> i64 {
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
	fn runs(&self) -> Vec<(Position, Option<Position>)> {
		let mut runs = Vec::new();
		walk(&self.root, 0, &mut runs);
		runs
	}

	/// Where the open span starts, if an open range remains.
	fn tail(&self) -> Option<Position> {
		if sum(&self.root) <= 0 {
			return None;
		}
		match last_zero(&self.root, 0) {
			Some(zero) => successor(&self.root, &zero),
			None => first(&self.root),
		}
	}
}

impl Node {
	fn new(key: Position, delta: i64, priority: u64) -> Box<Self> {
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
fn split(link: Link, key: &Position, inclusive: bool) -> (Link, Link) {
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

fn first(link: &Link) -> Option<Position> {
	let mut node = link.as_ref()?;
	while let Some(left) = &node.left {
		node = left;
	}
	Some(node.key)
}

/// The first key above `after`.
fn successor(mut link: &Link, after: &Position) -> Option<Position> {
	let mut found = None;
	while let Some(node) = link {
		if node.key > *after {
			found = Some(node.key);
			link = &node.left;
		} else {
			link = &node.right;
		}
	}
	found
}

/// Append the covered spans under `link`, entered with `offset` ranges open.
///
/// A subtree entered covered whose running sum never reaches zero holds no boundary of the
/// union, so it is skipped whole: the walk visits O(log n) nodes per span it returns.
fn walk(link: &Link, offset: i64, runs: &mut Vec<(Position, Option<Position>)>) {
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

/// The last key where the running sum, starting at `offset`, is zero.
fn last_zero(link: &Link, offset: i64) -> Option<Position> {
	let node = link.as_ref()?;
	if offset.saturating_add(node.low) > 0 {
		return None;
	}
	let at = offset + sum(&node.left) + node.delta;
	if let Some(key) = last_zero(&node.right, at) {
		return Some(key);
	}
	if at <= 0 {
		return Some(node.key);
	}
	last_zero(&node.left, offset)
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
		let anchor = |rng: &mut rand::rngs::StdRng| match rng.random_range(0..6) {
			0 => Anchor::Relative(rng.random_range(0..4)),
			1 => Anchor::Relative(u64::MAX),
			_ => Anchor::Absolute(Position {
				group: rng.random_range(0..12),
				frame: rng.random_range(0..3),
			}),
		};
		let mut ranges = Ranges::default();
		let mut held: Vec<Range> = Vec::new();
		for step in 0..50_000 {
			let grow = if held.len() < 24 { 0.65 } else { 0.35 };
			let before = union(held.iter().copied());
			let changed = if held.is_empty() || rng.random_bool(grow) {
				let range = Range {
					start: anchor(&mut rng),
					end: rng.random_bool(0.6).then(|| anchor(&mut rng)),
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
