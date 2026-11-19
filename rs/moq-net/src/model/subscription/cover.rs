//! Counted runs of group keys, persistent so a copy costs nothing and an edit costs its own
//! runs, never every other one.
//!
//! A subscription may name thousands of ranges, and an update may edit one of them. Each run
//! counts +1 at its start and -1 just past its end, so a key is covered while the running sum
//! above it stays positive. A treap keeps each subtree's sum and its lowest and highest
//! running sums, so adding or removing a run touches two keys and every query is one descent.
//! Nodes are shared between copies and copied only along the path an edit walks, and a key's
//! priority is the same in every copy, so two copies of one lineage share every subtree an
//! edit between them didn't touch: [`Cover::moved`] finds what changed by skipping those.

use std::{
	hash::{BuildHasher, RandomState},
	ops::RangeInclusive,
	sync::{Arc, LazyLock},
};

/// One seed per process, so a key ranks the same in every cover and copies stay comparable,
/// and so a peer can't pick keys that degrade the treap.
static PRIORITIES: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// A multiset of runs of keys, read as the set of keys any run covers.
#[derive(Clone, Default)]
pub(in crate::model) struct Cover {
	root: Link,
}

type Link = Option<Arc<Node>>;

#[derive(Clone)]
struct Node {
	key: u64,
	/// Runs starting here, minus runs ending just before here. Never zero.
	delta: i64,
	priority: u64,
	/// The sum of every delta in this subtree.
	sum: i64,
	/// The lowest and highest running sums at any key of this subtree, counted from its first.
	low: i64,
	high: i64,
	left: Link,
	right: Link,
}

impl PartialEq for Cover {
	/// The same copy, unedited since: two covers holding the same keys by different edits
	/// compare unequal, which costs a reader one spurious refresh at most.
	fn eq(&self, other: &Self) -> bool {
		match (&self.root, &other.root) {
			(None, None) => true,
			(Some(a), Some(b)) => Arc::ptr_eq(a, b),
			_ => false,
		}
	}
}

impl std::fmt::Debug for Cover {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_list().entries(self.runs(0)).finish()
	}
}

/// What a running sum is searched for.
#[derive(Clone, Copy)]
enum Want {
	/// Covered: above zero.
	Covered,
	/// Uncovered: zero, since no key counts below it.
	Gap,
}

impl Want {
	fn at(self, sum: i64) -> bool {
		match self {
			Self::Covered => sum > 0,
			Self::Gap => sum <= 0,
		}
	}

	/// Whether a subtree entered with `offset` may hold a match.
	fn within(self, offset: i64, node: &Node) -> bool {
		match self {
			Self::Covered => offset.saturating_add(node.high) > 0,
			Self::Gap => offset.saturating_add(node.low) <= 0,
		}
	}
}

impl Cover {
	/// The runs given, each counted once.
	#[cfg(test)]
	pub fn new(runs: impl IntoIterator<Item = RangeInclusive<u64>>) -> Self {
		let mut cover = Self::default();
		for run in runs {
			cover.add(run, 1);
		}
		cover
	}

	/// Count `delta` more of `run`, or fewer when negative: a run is removed only as often as
	/// it was added. An empty run counts nothing.
	pub fn add(&mut self, run: RangeInclusive<u64>, delta: i64) {
		let (start, end) = (*run.start(), *run.end());
		if start > end || delta == 0 {
			return;
		}
		self.shift(start, delta);
		if let Some(past) = end.checked_add(1) {
			self.shift(past, -delta);
		}
	}

	fn shift(&mut self, key: u64, delta: i64) {
		let (below, rest) = split(self.root.take(), key, false);
		let (at, above) = split(rest, key, true);
		let delta = at.map_or(0, |node| node.delta) + delta;
		let at = (delta != 0).then(|| Node::new(key, delta));
		self.root = merge(merge(below, at), above);
	}

	/// Whether no run is counted.
	#[cfg(test)]
	pub fn is_empty(&self) -> bool {
		self.root.is_none()
	}

	/// The running sum at `key`: how many runs cover it.
	fn count(&self, key: u64) -> i64 {
		let mut link = &self.root;
		let mut total = 0;
		while let Some(node) = link {
			if node.key <= key {
				total += sum(&node.left) + node.delta;
				link = &node.right;
			} else {
				link = &node.left;
			}
		}
		total
	}

	pub fn contains(&self, key: u64) -> bool {
		self.count(key) > 0
	}

	/// The first key at or after `from` whose running sum is `want`.
	fn first_key(&self, from: u64, want: Want) -> Option<u64> {
		first_from(&self.root, 0, from, want)
	}

	/// The last key at or before `to` whose running sum is `want`.
	fn last_key(&self, to: u64, want: Want) -> Option<u64> {
		last_to(&self.root, 0, to, want)
	}

	/// The first boundary key above `after`.
	fn successor(&self, after: u64) -> Option<u64> {
		let mut link = &self.root;
		let mut found = None;
		while let Some(node) = link {
			if node.key > after {
				found = Some(node.key);
				link = &node.left;
			} else {
				link = &node.right;
			}
		}
		found
	}

	/// Where the run covering `key` ends.
	fn run_end(&self, key: u64) -> u64 {
		match self.first_key(key, Want::Gap) {
			Some(gap) => gap - 1,
			None => u64::MAX,
		}
	}

	/// Where the run covering `key` starts.
	fn run_start(&self, key: u64) -> u64 {
		match self.last_key(key, Want::Gap) {
			Some(gap) => self.successor(gap).expect("a covered key past the gap"),
			None => first(&self.root).expect("a covered key"),
		}
	}

	/// The lowest run reaching into `lo..=hi`, its start clipped to `lo`.
	pub fn first(&self, lo: u64, hi: u64) -> Option<RangeInclusive<u64>> {
		if lo > hi {
			return None;
		}
		let start = match self.contains(lo) {
			true => lo,
			false => self.first_key(lo, Want::Covered).filter(|start| *start <= hi)?,
		};
		Some(start..=self.run_end(start))
	}

	/// The highest run reaching into `lo..=hi`, its end clipped to `hi`.
	pub fn last(&self, lo: u64, hi: u64) -> Option<RangeInclusive<u64>> {
		if lo > hi {
			return None;
		}
		let end = match self.contains(hi) {
			true => hi,
			false => self.run_end(self.last_key(hi, Want::Covered)?),
		};
		(end >= lo).then(|| self.run_start(end)..=end)
	}

	/// The runs from `from` on, lowest first, each whole but clipped to `from`.
	///
	/// One walk in key order that skips every subtree holding no run's edge, so each run
	/// costs about one descent, however many runs overlap inside it.
	pub fn runs(&self, from: u64) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
		Runs::new(self, from)
	}

	/// The runs reaching into `lo..=hi`, lowest first, clipped to it.
	pub fn within(&self, lo: u64, hi: u64) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
		self.runs(lo)
			.take_while(move |run| *run.start() <= hi)
			.map(move |run| *run.start()..=(*run.end()).min(hi))
	}

	/// The lowest key covered: the first edge, where the first run starts.
	pub fn lowest(&self) -> Option<u64> {
		first(&self.root)
	}

	/// The highest key covered: just below the last edge, where the last run ends, or every
	/// key on while a run is open.
	pub fn highest(&self) -> Option<u64> {
		match sum(&self.root) > 0 {
			true => Some(u64::MAX),
			false => last(&self.root).map(|key| key - 1),
		}
	}

	/// The spans where `self` and `old` may cover differently, lowest first: where their counts
	/// differ, a superset of where coverage does.
	///
	/// Costs the keys edited between them, times the depth, when one is an edited copy of the
	/// other: every subtree they still share is skipped. Unrelated covers cost both.
	pub fn moved(&self, old: &Self) -> Vec<RangeInclusive<u64>> {
		let mut changed = Vec::new();
		diff(self.root.clone(), old.root.clone(), &mut changed);
		changed.sort_unstable_by_key(|(key, _)| *key);
		// A key met on both sides of a split is listed once per side.
		changed.dedup_by(|next, kept| {
			let same = next.0 == kept.0;
			if same {
				kept.1 += next.1;
			}
			same
		});
		let mut spans = Vec::new();
		let mut open: Option<u64> = None;
		let mut difference = 0;
		for (key, delta) in changed {
			difference += delta;
			match (open, difference != 0) {
				(None, true) => open = Some(key),
				(Some(start), false) => {
					spans.push(start..=key - 1);
					open = None;
				}
				_ => {}
			}
		}
		spans.extend(open.map(|start| start..=u64::MAX));
		spans
	}

	/// What `self` covers and `old` doesn't within `lo..=hi`, lowest first. Costs what
	/// [`Self::moved`] does, and the runs inside where they differ.
	pub fn gained(&self, old: &Self, lo: u64, hi: u64) -> Vec<RangeInclusive<u64>> {
		let mut gained = Vec::new();
		if lo > hi {
			return gained;
		}
		for span in self.moved(old) {
			let (lo, hi) = ((*span.start()).max(lo), (*span.end()).min(hi));
			for run in self.within(lo, hi) {
				gained.extend(minus(run, old));
			}
		}
		gained
	}
}

/// The parts of `run` that `cover` doesn't hold, lowest first.
pub(in crate::model) fn minus(run: RangeInclusive<u64>, cover: &Cover) -> Vec<RangeInclusive<u64>> {
	let (lo, hi) = (*run.start(), *run.end());
	let mut gaps = Vec::new();
	let mut from = (lo <= hi).then_some(lo);
	for held in cover.within(lo, hi) {
		let Some(at) = from else { break };
		if *held.start() > at {
			gaps.push(at..=*held.start() - 1);
		}
		from = held.end().checked_add(1).filter(|next| *next <= hi);
	}
	gaps.extend(from.map(|at| at..=hi));
	gaps
}

impl Node {
	fn new(key: u64, delta: i64) -> Arc<Self> {
		Arc::new(Self {
			key,
			delta,
			priority: PRIORITIES.hash_one(key),
			sum: delta,
			low: delta,
			high: delta,
			left: None,
			right: None,
		})
	}

	fn update(&mut self) {
		let at = sum(&self.left) + self.delta;
		self.sum = at + sum(&self.right);
		let (left_low, left_high) = bounds(&self.left);
		let (right_low, right_high) = bounds(&self.right);
		self.low = left_low.min(at).min(at.saturating_add(right_low));
		self.high = left_high.max(at).max(at.saturating_add(right_high));
	}
}

fn sum(link: &Link) -> i64 {
	link.as_ref().map_or(0, |node| node.sum)
}

/// A subtree's lowest and highest running sums, neutral for an empty one.
fn bounds(link: &Link) -> (i64, i64) {
	link.as_ref().map_or((i64::MAX, i64::MIN), |node| (node.low, node.high))
}

/// Split into the keys below `key` (through it when `inclusive`) and the rest, copying the
/// nodes on the path only where another cover still shares them.
fn split(link: Link, key: u64, inclusive: bool) -> (Link, Link) {
	let Some(mut node) = link else {
		return (None, None);
	};
	let inner = Arc::make_mut(&mut node);
	if inner.key < key || (inclusive && inner.key == key) {
		let (below, rest) = split(inner.right.take(), key, inclusive);
		inner.right = below;
		inner.update();
		(Some(node), rest)
	} else {
		let (rest, above) = split(inner.left.take(), key, inclusive);
		inner.left = above;
		inner.update();
		(rest, Some(node))
	}
}

/// Join two treaps, every key of `left` below every key of `right`.
fn merge(left: Link, right: Link) -> Link {
	match (left, right) {
		(None, link) | (link, None) => link,
		(Some(mut left), Some(mut right)) => {
			if left.priority > right.priority {
				let inner = Arc::make_mut(&mut left);
				inner.right = merge(inner.right.take(), Some(right));
				inner.update();
				Some(left)
			} else {
				let inner = Arc::make_mut(&mut right);
				inner.left = merge(Some(left), inner.left.take());
				inner.update();
				Some(right)
			}
		}
	}
}

fn first(link: &Link) -> Option<u64> {
	let mut node = link.as_ref()?;
	while let Some(left) = &node.left {
		node = left;
	}
	Some(node.key)
}

fn last(link: &Link) -> Option<u64> {
	let mut node = link.as_ref()?;
	while let Some(right) = &node.right {
		node = right;
	}
	Some(node.key)
}

/// [`Cover::runs`]: the edges above where it started, in key order, each node waiting with
/// how many runs were open entering it.
struct Runs<'a> {
	stack: Vec<(&'a Node, i64)>,
	/// Where the run being walked started, `None` between runs.
	open: Option<u64>,
	done: bool,
}

impl<'a> Runs<'a> {
	fn new(cover: &'a Cover, from: u64) -> Self {
		let mut runs = Self {
			stack: Vec::new(),
			open: (cover.count(from) > 0).then_some(from),
			done: false,
		};
		// Every edge above `from`, the ones at or below it counted into where it starts.
		let mut link = &cover.root;
		let mut offset = 0;
		while let Some(node) = link {
			if node.key <= from {
				offset += sum(&node.left) + node.delta;
				link = &node.right;
			} else {
				runs.stack.push((node, offset));
				link = &node.left;
			}
		}
		runs
	}

	/// Queue the subtree under `link`, entered with `offset` runs open, down its left edge,
	/// leaving out each subtree whose running sum stays on the side it entered on.
	fn descend(&mut self, mut link: &'a Link, offset: i64) {
		while let Some(node) = link {
			let crosses = match offset > 0 {
				true => offset.saturating_add(node.low) <= 0,
				false => offset.saturating_add(node.high) > 0,
			};
			if !crosses {
				return;
			}
			self.stack.push((node, offset));
			link = &node.left;
		}
	}
}

impl Iterator for Runs<'_> {
	type Item = RangeInclusive<u64>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.done {
			return None;
		}
		while let Some((node, offset)) = self.stack.pop() {
			let at = offset + sum(&node.left) + node.delta;
			self.descend(&node.right, at);
			match self.open {
				Some(start) if at <= 0 => {
					self.open = None;
					return Some(start..=node.key - 1);
				}
				None if at > 0 => self.open = Some(node.key),
				_ => {}
			}
		}
		self.done = true;
		self.open.take().map(|start| start..=u64::MAX)
	}
}

/// The first key of the subtree, entered with `offset` runs open, whose running sum is `want`.
fn first_in(link: &Link, offset: i64, want: Want) -> Option<u64> {
	let node = link.as_ref()?;
	if !want.within(offset, node) {
		return None;
	}
	if let Some(key) = first_in(&node.left, offset, want) {
		return Some(key);
	}
	let at = offset + sum(&node.left) + node.delta;
	if want.at(at) {
		return Some(node.key);
	}
	first_in(&node.right, at, want)
}

/// [`first_in`] over the keys at or after `from`.
fn first_from(link: &Link, offset: i64, from: u64, want: Want) -> Option<u64> {
	let node = link.as_ref()?;
	let at = offset + sum(&node.left) + node.delta;
	if node.key < from {
		return first_from(&node.right, at, from, want);
	}
	if let Some(key) = first_from(&node.left, offset, from, want) {
		return Some(key);
	}
	if want.at(at) {
		return Some(node.key);
	}
	first_in(&node.right, at, want)
}

/// The last key of the subtree, entered with `offset` runs open, whose running sum is `want`.
fn last_in(link: &Link, offset: i64, want: Want) -> Option<u64> {
	let node = link.as_ref()?;
	if !want.within(offset, node) {
		return None;
	}
	let at = offset + sum(&node.left) + node.delta;
	if let Some(key) = last_in(&node.right, at, want) {
		return Some(key);
	}
	if want.at(at) {
		return Some(node.key);
	}
	last_in(&node.left, offset, want)
}

/// [`last_in`] over the keys at or before `to`.
fn last_to(link: &Link, offset: i64, to: u64, want: Want) -> Option<u64> {
	let node = link.as_ref()?;
	if node.key > to {
		return last_to(&node.left, offset, to, want);
	}
	let at = offset + sum(&node.left) + node.delta;
	if let Some(key) = last_to(&node.right, at, to, want) {
		return Some(key);
	}
	if want.at(at) {
		return Some(node.key);
	}
	last_in(&node.left, offset, want)
}

/// Collect each key whose delta differs between `new` and `old`, with how much more `new`
/// counts there. A key's priority is fixed, so the higher-ranked root of the two holds a key
/// the other lacks, unless both roots are the same key.
fn diff(new: Link, old: Link, changed: &mut Vec<(u64, i64)>) {
	match (new, old) {
		(None, None) => {}
		(Some(a), Some(b)) if Arc::ptr_eq(&a, &b) => {}
		(Some(a), None) => collect(&Some(a), 1, changed),
		(None, Some(b)) => collect(&Some(b), -1, changed),
		(Some(a), Some(b)) if a.key == b.key => {
			if a.delta != b.delta {
				changed.push((a.key, a.delta - b.delta));
			}
			diff(a.left.clone(), b.left.clone(), changed);
			diff(a.right.clone(), b.right.clone(), changed);
		}
		(Some(a), Some(b)) if a.priority > b.priority => {
			changed.push((a.key, a.delta));
			let (below, above) = split(Some(b), a.key, false);
			diff(a.left.clone(), below, changed);
			diff(a.right.clone(), above, changed);
		}
		(Some(a), Some(b)) => {
			changed.push((b.key, -b.delta));
			let (below, above) = split(Some(a), b.key, false);
			diff(below, b.left.clone(), changed);
			diff(above, b.right.clone(), changed);
		}
	}
}

/// Every key of the subtree, its delta scaled by `sign`.
fn collect(link: &Link, sign: i64, changed: &mut Vec<(u64, i64)>) {
	if let Some(node) = link {
		collect(&node.left, sign, changed);
		changed.push((node.key, sign * node.delta));
		collect(&node.right, sign, changed);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rangemap::RangeInclusiveSet;

	/// A small deterministic generator, so a failure replays.
	fn generator() -> impl FnMut(u64) -> u64 {
		let mut x = 0x9e37_79b9_7f4a_7c15_u64;
		move |bound| {
			x ^= x << 13;
			x ^= x >> 7;
			x ^= x << 17;
			x % bound
		}
	}

	fn random_run(next: &mut impl FnMut(u64) -> u64, span: u64) -> RangeInclusive<u64> {
		let start = match next(16) {
			0 => 0,
			_ => next(span),
		};
		// Now and then a run reaching the last key.
		let end = match next(8) {
			0 => u64::MAX,
			_ => start + next(4),
		};
		start..=end
	}

	/// The points a cover is probed at: each bound of every run, either side of it.
	fn points(runs: &[RangeInclusive<u64>]) -> Vec<u64> {
		let mut points: Vec<u64> = runs
			.iter()
			.flat_map(|run| [*run.start(), *run.end()])
			.flat_map(|point| [point.saturating_sub(1), point, point.saturating_add(1)])
			.chain([0, u64::MAX])
			.collect();
		points.sort_unstable();
		points.dedup();
		points
	}

	/// A cover answers every query as the set of the runs it holds would, through any
	/// sequence of adds and removes, and a copy taken before an edit is left as it was.
	#[test]
	fn a_cover_matches_the_union_of_its_runs() {
		let mut next = generator();
		for round in 0..400 {
			let mut cover = Cover::default();
			let mut held: Vec<RangeInclusive<u64>> = Vec::new();
			for step in 0..24 {
				let before = (cover.clone(), held.clone());
				if held.is_empty() || next(3) != 0 {
					let run = random_run(&mut next, 40);
					cover.add(run.clone(), 1);
					held.push(run);
				} else {
					let run = held.swap_remove(next(held.len() as u64) as usize);
					cover.add(run, -1);
				}
				let union: RangeInclusiveSet<u64> = held.iter().cloned().collect();
				let context = || format!("round {round} step {step}: {held:?}");
				assert!(cover.runs(0).eq(union.iter().cloned()), "{}", context());
				assert_eq!(cover.is_empty(), held.is_empty(), "{}", context());
				let all = points(&union.iter().cloned().collect::<Vec<_>>());
				assert_eq!(cover.lowest(), union.iter().next().map(|run| *run.start()), "{}", context());
				assert_eq!(cover.highest(), union.iter().next_back().map(|run| *run.end()), "{}", context());
				for &lo in &all {
					assert_eq!(cover.contains(lo), union.contains(&lo), "{} at {lo}", context());
					let from = union
						.overlapping(lo..=u64::MAX)
						.map(|run| (*run.start()).max(lo)..=*run.end());
					assert!(cover.runs(lo).eq(from), "{} runs from {lo}", context());
					for &hi in all.iter().filter(|hi| **hi >= lo) {
						let first = union
							.overlapping(lo..=hi)
							.next()
							.map(|run| (*run.start()).max(lo)..=*run.end());
						assert_eq!(cover.first(lo, hi), first, "{} first in {lo}..={hi}", context());
						let last = union
							.overlapping(lo..=hi)
							.next_back()
							.map(|run| *run.start()..=(*run.end()).min(hi));
						assert_eq!(cover.last(lo, hi), last, "{} last in {lo}..={hi}", context());
					}
				}
				// The copy before the edit still answers as it did.
				let (old, old_held) = before;
				let old_union: RangeInclusiveSet<u64> = old_held.iter().cloned().collect();
				assert!(old.runs(0).eq(old_union.iter().cloned()), "{}", context());
				// What moved covers every key whose coverage changed, and what was gained is
				// exactly the keys newly covered.
				let moved: RangeInclusiveSet<u64> = cover.moved(&old).into_iter().collect();
				let gained: RangeInclusiveSet<u64> = cover.gained(&old, 0, u64::MAX).into_iter().collect();
				for &key in &all {
					let (now, then) = (union.contains(&key), old_union.contains(&key));
					assert!(now == then || moved.contains(&key), "{} moved at {key}", context());
					assert_eq!(gained.contains(&key), now && !then, "{} gained at {key}", context());
				}
			}
		}
	}

	/// An edit to a large cover moves exactly the span it touched, and undoing it moves nothing.
	#[test]
	fn an_edit_moves_only_what_it_touched() {
		let mut cover = Cover::new((0..10_000u64).map(|key| 4 * key..=4 * key + 1));
		let old = cover.clone();
		cover.add(20_001..=20_002, 1);
		assert_eq!(cover.moved(&old), vec![20_001..=20_002]);
		assert_eq!(cover.gained(&old, 0, u64::MAX), vec![20_002..=20_002]);
		cover.add(20_001..=20_002, -1);
		assert!(cover.moved(&old).is_empty());
	}

	/// A run minus a cover is the parts of it the cover doesn't hold.
	#[test]
	fn a_run_minus_a_cover_is_its_gaps() {
		let mut next = generator();
		for _ in 0..2000 {
			let runs: Vec<_> = (0..next(7)).map(|_| random_run(&mut next, 40)).collect();
			let cover = Cover::new(runs.clone());
			let start = next(40);
			let run = start..=start + next(10);
			let expected: Vec<u64> = run.clone().filter(|key| !cover.contains(*key)).collect();
			let found: Vec<u64> = minus(run.clone(), &cover).into_iter().flatten().collect();
			assert_eq!(found, expected, "{run:?} minus {runs:?}");
		}
		let whole = Cover::new(std::iter::once(0..=u64::MAX));
		assert!(minus(0..=u64::MAX, &whole).is_empty());
		let low = Cover::new(std::iter::once(0..=9));
		assert_eq!(minus(5..=u64::MAX, &low), vec![10..=u64::MAX]);
	}
}
