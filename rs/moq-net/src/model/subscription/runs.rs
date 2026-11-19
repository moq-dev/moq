//! Sets of group keys held as sorted runs, and the layered set a [`Window`](super::Window)
//! reads: fixed runs shared across its moves, with the few runs its time ends walk on top.
//!
//! A subscription may name thousands of ranges, yet each time start that resolves, or each
//! group a drop gives up under one, widens a single range. Rebuilding or copying every run
//! on each such change would let a peer multiply what one group costs by its range count,
//! so the runs that don't move are kept apart, shared by reference, and only the few runs
//! layered above them are copied.

use std::{ops::RangeInclusive, sync::Arc};

/// Disjoint runs of groups, lowest first and never adjacent, each found from either end by
/// one search.
///
/// Not a [`RangeInclusiveSet`](rangemap::RangeInclusiveSet): its overlapping iterator walks
/// back from the last run, so finding the highest run below a point costs every run above it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::model) struct Runs(Vec<RangeInclusive<u64>>);

impl Runs {
	/// The runs of `runs`, in any order, overlapping or not.
	pub fn new(runs: impl IntoIterator<Item = RangeInclusive<u64>>) -> Self {
		let mut runs: Vec<_> = runs.into_iter().filter(|run| !run.is_empty()).collect();
		runs.sort_unstable_by_key(|run| *run.start());
		Self(coalesce(runs))
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Add `run`, merging it with every run it overlaps or touches: a search and a copy of
	/// the runs above it.
	pub fn insert(&mut self, run: RangeInclusive<u64>) {
		if run.is_empty() {
			return;
		}
		let (lo, hi) = (*run.start(), *run.end());
		let from = self.0.partition_point(|run| run.end().saturating_add(1) < lo);
		let to = self.0.partition_point(|run| *run.start() <= hi.saturating_add(1));
		let merged = match self.0.get(from..to) {
			Some([first, .., last]) => lo.min(*first.start())..=hi.max(*last.end()),
			Some([only]) => lo.min(*only.start())..=hi.max(*only.end()),
			_ => run,
		};
		self.0.splice(from..to, std::iter::once(merged));
	}

	/// The runs of both, merged in one pass over each.
	fn union(&self, other: &Self) -> Self {
		let (mut a, mut b) = (self.0.iter().peekable(), other.0.iter().peekable());
		let merged = std::iter::from_fn(|| match (a.peek(), b.peek()) {
			(Some(x), Some(y)) if x.start() <= y.start() => a.next(),
			(Some(_), Some(_)) => b.next(),
			(Some(_), None) => a.next(),
			(None, _) => b.next(),
		});
		Self(coalesce(merged.cloned().collect()))
	}

	pub fn contains(&self, key: u64) -> bool {
		let index = self.0.partition_point(|run| *run.end() < key);
		self.0.get(index).is_some_and(|run| *run.start() <= key)
	}

	/// The lowest run reaching into `lo..=hi`.
	pub fn first(&self, lo: u64, hi: u64) -> Option<&RangeInclusive<u64>> {
		let index = self.0.partition_point(|run| *run.end() < lo);
		self.0.get(index).filter(|run| *run.start() <= hi)
	}

	/// The highest run reaching into `lo..=hi`.
	pub fn last(&self, lo: u64, hi: u64) -> Option<&RangeInclusive<u64>> {
		let index = self.0.partition_point(|run| *run.start() <= hi);
		self.0.get(index.checked_sub(1)?).filter(|run| *run.end() >= lo)
	}

	/// The runs reaching into `lo..=hi`, lowest first, clipped to it.
	pub fn within(&self, lo: u64, hi: u64) -> impl DoubleEndedIterator<Item = RangeInclusive<u64>> + '_ {
		let from = self.0.partition_point(|run| *run.end() < lo);
		let to = self.0.partition_point(|run| *run.start() <= hi).max(from);
		self.0[from..to]
			.iter()
			.map(move |run| (*run.start()).max(lo)..=(*run.end()).min(hi))
	}
}

/// A set of groups held as shared fixed runs with a layer of runs over them, which may
/// overlap them or each other.
///
/// Each query answers from both by searches, never by visiting the fixed runs a layered run
/// covers, so a layer that moves costs its own runs, however many fixed ones lie under it.
/// Both are shared, so a copy costs nothing until the layer moves.
#[derive(Clone, Debug, Default)]
pub(in crate::model) struct Layered {
	fixed: Arc<Runs>,
	layer: Arc<Runs>,
}

impl PartialEq for Layered {
	/// Equal as held: the same fixed runs and layer, each compared by reference first. The
	/// same groups split differently between the two compare unequal.
	fn eq(&self, other: &Self) -> bool {
		let same = |a: &Arc<Runs>, b: &Arc<Runs>| Arc::ptr_eq(a, b) || a == b;
		same(&self.fixed, &other.fixed) && same(&self.layer, &other.layer)
	}
}

impl Layered {
	pub fn new(fixed: Arc<Runs>, layer: Runs) -> Self {
		Self {
			fixed,
			layer: Arc::new(layer),
		}
	}

	pub fn fixed(&self) -> &Arc<Runs> {
		&self.fixed
	}

	pub fn layer(&self) -> &Runs {
		&self.layer
	}

	/// Whether both hold the same fixed runs, so they differ only in their layers.
	pub fn shares_fixed(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.fixed, &other.fixed)
	}

	pub fn contains(&self, key: u64) -> bool {
		self.fixed.contains(key) || self.layer.contains(key)
	}

	/// The one set holding runs, when the other holds none: its runs are the union's, each
	/// already whole, so a read needs no growing across the two.
	fn only(&self) -> Option<&Runs> {
		match (self.fixed.0.is_empty(), self.layer.0.is_empty()) {
			(_, true) => Some(&self.fixed),
			(true, false) => Some(&self.layer),
			(false, false) => None,
		}
	}

	/// The lowest whole run of the union reaching into `lo..=hi`, its start clipped to `lo`.
	pub fn first(&self, lo: u64, hi: u64) -> Option<RangeInclusive<u64>> {
		// Most windows have no layer: their fixed runs are already the union's.
		if let Some(only) = self.only() {
			return only.first(lo, hi).map(|run| (*run.start()).max(lo)..=*run.end());
		}
		let starts = [self.fixed.first(lo, hi), self.layer.first(lo, hi)];
		let start = starts.into_iter().flatten().map(|run| (*run.start()).max(lo)).min()?;
		// Grow the end through whatever either set holds next to it: each step crosses into
		// the other set, so a layered run over many fixed ones costs a search, not a walk.
		let mut end = start;
		loop {
			let grown = [&*self.fixed, &*self.layer]
				.into_iter()
				.filter_map(|runs| runs.first(end, end.saturating_add(1)))
				.map(|run| *run.end())
				.max()
				.filter(|grown| *grown > end);
			match grown {
				Some(grown) => end = grown,
				None => return Some(start..=end),
			}
			if end == u64::MAX {
				return Some(start..=end);
			}
		}
	}

	/// The highest whole run of the union reaching into `lo..=hi`, its end clipped to `hi`.
	pub fn last(&self, lo: u64, hi: u64) -> Option<RangeInclusive<u64>> {
		if let Some(only) = self.only() {
			return only.last(lo, hi).map(|run| *run.start()..=(*run.end()).min(hi));
		}
		let ends = [self.fixed.last(lo, hi), self.layer.last(lo, hi)];
		let end = ends.into_iter().flatten().map(|run| (*run.end()).min(hi)).max()?;
		let mut start = end;
		loop {
			let grown = [&*self.fixed, &*self.layer]
				.into_iter()
				.filter_map(|runs| runs.last(start.saturating_sub(1), start))
				.map(|run| *run.start())
				.min()
				.filter(|grown| *grown < start);
			match grown {
				Some(grown) => start = grown,
				None => return Some(start..=end),
			}
			if start == 0 {
				return Some(start..=end);
			}
		}
	}

	/// The union's runs from `from` on, lowest first, each whole but clipped to `from`.
	pub fn runs(&self, from: u64) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
		let mut next = Some(from);
		std::iter::from_fn(move || {
			let run = self.first(next?, u64::MAX)?;
			next = run.end().checked_add(1);
			Some(run)
		})
	}

	/// The lowest group held.
	pub fn lowest(&self) -> Option<u64> {
		self.first(0, u64::MAX).map(|run| *run.start())
	}

	/// The highest group held.
	pub fn highest(&self) -> Option<u64> {
		self.last(0, u64::MAX).map(|run| *run.end())
	}

	/// Add `run`. The layer takes it, so what reads both diffs only the layers, until the
	/// layer outgrows the square root of the fixed runs and folds into them: each add costs
	/// that root, copying the layer and folding included, however many ranges lie under it.
	/// A run already held changes nothing, so the result stays equal by reference.
	pub fn insert(&mut self, run: RangeInclusive<u64>) {
		let (lo, hi) = (*run.start(), *run.end());
		if lo > hi
			|| self
				.first(lo, hi)
				.is_some_and(|held| *held.start() == lo && *held.end() >= hi)
		{
			return;
		}
		let layer = Arc::make_mut(&mut self.layer);
		layer.insert(run);
		if layer.len().saturating_mul(layer.len()) > self.fixed.len() {
			self.fixed = Arc::new(self.fixed.union(layer));
			self.layer = Arc::default();
		}
	}

	/// What `self` holds and `old` doesn't within `lo..=hi`, lowest first, when both share
	/// their fixed runs: `None` when they don't.
	///
	/// Costs the two layers, and the fixed runs inside where they differ: a layered run that
	/// grows over many fixed ones, from the live edge to past every group, costs them only
	/// within `lo..=hi`.
	pub fn gained(&self, old: &Self, lo: u64, hi: u64) -> Option<Vec<RangeInclusive<u64>>> {
		if !self.shares_fixed(old) {
			return None;
		}
		let mut gained = Vec::new();
		if lo > hi {
			return Some(gained);
		}
		for run in self.layer.within(lo, hi) {
			for piece in minus(run, &old.layer) {
				gained.extend(minus(piece, &self.fixed));
			}
		}
		Some(gained)
	}
}

/// `runs`, sorted by start, with each run that overlaps or touches the one before merged
/// into it.
fn coalesce(runs: Vec<RangeInclusive<u64>>) -> Vec<RangeInclusive<u64>> {
	let mut merged: Vec<RangeInclusive<u64>> = Vec::with_capacity(runs.len());
	for run in runs {
		match merged.last_mut() {
			Some(last) if *run.start() <= last.end().saturating_add(1) => {
				if run.end() > last.end() {
					*last = *last.start()..=*run.end();
				}
			}
			_ => merged.push(run),
		}
	}
	merged
}

/// The parts of `run` that `runs` doesn't hold, lowest first.
fn minus(run: RangeInclusive<u64>, runs: &Runs) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
	let (lo, hi) = (*run.start(), *run.end());
	let mut from = (lo <= hi).then_some(lo);
	let mut inside = runs.within(lo, hi).peekable();
	std::iter::from_fn(move || {
		loop {
			let at = from?;
			match inside.peek() {
				Some(held) if *held.start() <= at => {
					from = held.end().checked_add(1).filter(|next| *next <= hi);
					inside.next();
				}
				Some(held) => {
					let gap = at..=*held.start() - 1;
					from = held.end().checked_add(1).filter(|next| *next <= hi);
					inside.next();
					return Some(gap);
				}
				None => {
					from = None;
					return Some(at..=hi);
				}
			}
		}
	})
}

#[cfg(test)]
mod tests {
	use rangemap::RangeInclusiveSet;

	use super::*;

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

	fn random_runs(next: &mut impl FnMut(u64) -> u64, count: u64, span: u64) -> Vec<RangeInclusive<u64>> {
		(0..next(count + 1))
			.map(|_| {
				let start = next(span);
				// Now and then a run reaching the last key.
				let end = match next(8) {
					0 => u64::MAX,
					_ => start + next(4),
				};
				start..=end
			})
			.collect()
	}

	/// The points a layered set is probed at: each bound of every run, either side of it.
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

	/// Adding runs one at a time holds their union, folds the layer into the fixed runs once
	/// it outgrows their square root, and changes nothing for a run already held.
	#[test]
	fn inserted_runs_hold_their_union() {
		let mut next = generator();
		for round in 0..500 {
			let fixed = random_runs(&mut next, 40, 200);
			let mut layered = Layered::new(Arc::new(Runs::new(fixed.clone())), Runs::default());
			let mut union: RangeInclusiveSet<u64> = fixed.iter().cloned().collect();
			for run in random_runs(&mut next, 30, 200) {
				let before = layered.clone();
				let held = union.gaps(&run).next().is_none();
				layered.insert(run.clone());
				union.insert(run.clone());
				let context = || format!("round {round}: {fixed:?} after {run:?}");
				assert!(layered.runs(0).eq(union.iter().cloned()), "{}", context());
				assert!(layered.layer().len().pow(2) <= layered.fixed().len(), "{}", context());
				if held {
					assert!(Arc::ptr_eq(&before.layer, &layered.layer), "{}", context());
				}
			}
		}
	}

	/// A layered set answers every query as the set of its runs merged would.
	#[test]
	fn layered_runs_match_their_union() {
		let mut next = generator();
		for round in 0..2000 {
			let fixed = random_runs(&mut next, 6, 40);
			let layer = random_runs(&mut next, 3, 40);
			let layered = Layered::new(Arc::new(Runs::new(fixed.clone())), Runs::new(layer.clone()));
			let union: RangeInclusiveSet<u64> = fixed.iter().chain(&layer).cloned().collect();
			let context = || format!("round {round}: {fixed:?} under {layer:?}");
			assert!(layered.runs(0).eq(union.iter().cloned()), "{}", context());
			let points = points(&union.iter().cloned().collect::<Vec<_>>());
			for &lo in &points {
				assert_eq!(layered.contains(lo), union.contains(&lo), "{} at {lo}", context());
				let from: Vec<_> = union
					.overlapping(lo..=u64::MAX)
					.map(|run| (*run.start()).max(lo)..=*run.end())
					.collect();
				assert!(layered.runs(lo).eq(from), "{} from {lo}", context());
				for &hi in points.iter().filter(|hi| **hi >= lo) {
					let mut reaching = union.overlapping(lo..=hi);
					let first = reaching.next().map(|run| (*run.start()).max(lo)..=*run.end());
					assert_eq!(layered.first(lo, hi), first, "{} first in {lo}..={hi}", context());
					let last = union
						.overlapping(lo..=hi)
						.next_back()
						.map(|run| *run.start()..=(*run.end()).min(hi));
					assert_eq!(layered.last(lo, hi), last, "{} last in {lo}..={hi}", context());
				}
			}
		}
	}

	/// What a moved layer gains is exactly the groups the new set holds and the old one
	/// doesn't, within the span asked about.
	#[test]
	fn a_moved_layer_gains_exactly_the_new_groups() {
		let mut next = generator();
		for round in 0..2000 {
			let fixed = Arc::new(Runs::new(random_runs(&mut next, 6, 40)));
			let old = Layered::new(fixed.clone(), Runs::new(random_runs(&mut next, 3, 40)));
			let new = Layered::new(fixed, Runs::new(random_runs(&mut next, 3, 40)));
			let (lo, hi) = (next(30), 20 + next(30));
			let gained: RangeInclusiveSet<u64> = new.gained(&old, lo, hi).unwrap().into_iter().collect();
			let expected: Vec<u64> = (lo..=hi)
				.filter(|key| new.contains(*key) && !old.contains(*key))
				.collect();
			let found: Vec<u64> = (lo..=hi).filter(|key| gained.contains(key)).collect();
			assert_eq!(found, expected, "round {round}: {old:?} to {new:?} in {lo}..={hi}");
		}
		let unshared = Layered::new(Arc::new(Runs::default()), Runs::default());
		assert!(unshared.gained(&Layered::default(), 0, 9).is_none());
	}

	/// A run minus a set is the parts of it the set doesn't hold.
	#[test]
	fn a_run_minus_runs_is_its_gaps() {
		let mut next = generator();
		for _ in 0..2000 {
			let runs = Runs::new(random_runs(&mut next, 6, 40));
			let start = next(40);
			let run = start..=start + next(10);
			let expected: Vec<u64> = run.clone().filter(|key| !runs.contains(*key)).collect();
			let found: Vec<u64> = minus(run.clone(), &runs).flatten().collect();
			assert_eq!(found, expected, "{run:?} minus {runs:?}");
		}
		let whole = Runs::new(std::iter::once(0..=u64::MAX));
		assert!(minus(0..=u64::MAX, &whole).next().is_none());
		let low = Runs::new(std::iter::once(0..=9));
		let above: Vec<_> = minus(5..=u64::MAX, &low).collect();
		assert_eq!((above.len(), above.first()), (1, Some(&(10..=u64::MAX))));
	}
}
