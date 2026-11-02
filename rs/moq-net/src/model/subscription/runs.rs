//! Sets of group sequences held as sorted runs, and the layered set a [`Window`](super::Window)
//! reads: fixed runs shared across its moves, with the few runs its time ends walk on top.
//!
//! A subscription may name thousands of ranges, yet an unsettled time end moves its cap on
//! each group. Rebuilding or copying every run on each move would let a peer multiply what
//! one group costs by its range count, so the runs that never move are kept apart, shared
//! by reference, and only the layer above them is rebuilt.

use std::{ops::RangeInclusive, sync::Arc};

use rangemap::RangeInclusiveSet;

/// Disjoint runs of groups, lowest first and never adjacent, each found from either end by
/// one search.
///
/// Not a [`RangeInclusiveSet`]: its overlapping iterator walks back from the last run, so
/// finding the highest run below a point costs every run above it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::model) struct Runs(Vec<RangeInclusive<u64>>);

impl Runs {
	/// The runs of `runs`, in any order, overlapping or not.
	pub fn new(runs: impl IntoIterator<Item = RangeInclusive<u64>>) -> Self {
		let set: RangeInclusiveSet<u64> = runs.into_iter().filter(|run| !run.is_empty()).collect();
		Self(set.into_iter().collect())
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn iter(&self) -> std::slice::Iter<'_, RangeInclusive<u64>> {
		self.0.iter()
	}

	pub fn contains(&self, sequence: u64) -> bool {
		let index = self.0.partition_point(|run| *run.end() < sequence);
		self.0.get(index).is_some_and(|run| *run.start() <= sequence)
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

	/// Add `run`, merging the runs it overlaps or touches. Costs a search and a shift.
	pub fn insert(&mut self, run: RangeInclusive<u64>) {
		if run.is_empty() {
			return;
		}
		let (mut start, mut end) = run.into_inner();
		// The first run reaching `start - 1` or later, and the first starting past `end + 1`.
		let from = self.0.partition_point(|run| run.end().saturating_add(1) < start);
		let to = self
			.0
			.partition_point(|run| *run.start() <= end.saturating_add(1))
			.max(from);
		if let (Some(first), Some(last)) = (self.0[from..to].first(), self.0[from..to].last()) {
			start = start.min(*first.start());
			end = end.max(*last.end());
		}
		self.0.splice(from..to, std::iter::once(start..=end));
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

	/// The fixed runs to change, copied first if another set still shares them, so that set
	/// keeps what it held and no longer shares them.
	pub fn fixed_mut(&mut self) -> &mut Runs {
		Arc::make_mut(&mut self.fixed)
	}

	pub fn set_layer(&mut self, layer: Runs) {
		self.layer = Arc::new(layer);
	}

	/// Whether both hold the same fixed runs, so they differ only in their layers.
	pub fn shares_fixed(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.fixed, &other.fixed)
	}

	pub fn is_empty(&self) -> bool {
		self.fixed.is_empty() && self.layer.is_empty()
	}

	pub fn contains(&self, sequence: u64) -> bool {
		self.fixed.contains(sequence) || self.layer.contains(sequence)
	}

	/// The lowest whole run of the union reaching into `lo..=hi`, its start clipped to `lo`.
	pub fn first(&self, lo: u64, hi: u64) -> Option<RangeInclusive<u64>> {
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

/// The parts of `run` that `runs` doesn't hold, lowest first.
pub(in crate::model) fn minus(run: RangeInclusive<u64>, runs: &Runs) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
	let (lo, hi) = (*run.start(), *run.end());
	let mut from = Some(lo).filter(|_| lo <= hi);
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
				// Now and then a run reaching the last sequence.
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
						.last()
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
				.filter(|sequence| new.contains(*sequence) && !old.contains(*sequence))
				.collect();
			let found: Vec<u64> = (lo..=hi).filter(|sequence| gained.contains(sequence)).collect();
			assert_eq!(found, expected, "round {round}: {old:?} to {new:?} in {lo}..={hi}");
		}
		let unshared = Layered::new(Arc::new(Runs::default()), Runs::default());
		assert!(unshared.gained(&Layered::default(), 0, 9).is_none());
	}

	/// Inserting a run merges what it overlaps or touches, as a set of the runs would.
	#[test]
	fn inserted_runs_merge_as_a_set() {
		let mut next = generator();
		for _ in 0..2000 {
			let runs = random_runs(&mut next, 8, 40);
			let mut inserted = Runs::default();
			for run in &runs {
				inserted.insert(run.clone());
			}
			assert_eq!(inserted, Runs::new(runs.clone()), "{runs:?}");
		}
	}

	/// A run minus a set is the parts of it the set doesn't hold.
	#[test]
	fn a_run_minus_runs_is_its_gaps() {
		let mut next = generator();
		for _ in 0..2000 {
			let runs = Runs::new(random_runs(&mut next, 6, 40));
			let start = next(40);
			let run = start..=start + next(10);
			let expected: Vec<u64> = run.clone().filter(|sequence| !runs.contains(*sequence)).collect();
			let found: Vec<u64> = minus(run.clone(), &runs).flatten().collect();
			assert_eq!(found, expected, "{run:?} minus {runs:?}");
		}
		assert!(minus(0..=u64::MAX, &Runs::new([0..=u64::MAX])).next().is_none());
		assert_eq!(
			minus(5..=u64::MAX, &Runs::new([0..=9])).collect::<Vec<_>>(),
			vec![10..=u64::MAX]
		);
	}
}
