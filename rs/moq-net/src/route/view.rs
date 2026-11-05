//! An origin's claims as one node knows them, certified per region by floors.

use std::collections::{BTreeMap, BTreeSet};

use crate::Path;

use super::Rev;

/// Whether `path` is `prefix` or lies below it, matching whole segments.
pub(crate) fn within(path: &Path, prefix: &Path) -> bool {
	path.has_prefix(prefix)
}

/// Whether a claim on `claim` can concern a path under `region`.
///
/// Every claim covers its own path and everything below it, so a claim meets a
/// region when either lies within the other.
pub(crate) fn overlaps(claim: &Path, region: &Path) -> bool {
	within(claim, region) || within(region, claim)
}

/// What a node knows of one origin's claims.
///
/// Each region's floor certifies that the claims overlapping it are exactly the
/// origin's claims there as of that revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View<A> {
	pub floors: BTreeMap<Path, Rev>,
	pub items: BTreeMap<Path, A>,
}

impl<A> Default for View<A> {
	fn default() -> Self {
		Self {
			floors: BTreeMap::new(),
			items: BTreeMap::new(),
		}
	}
}

impl<A: Clone + Eq> View<A> {
	/// The view as a stream to a peer interested in `prefix` carries it.
	///
	/// A region meeting the interest is narrowed to whichever of the two lies
	/// within the other, so every region a stream carries lies within its
	/// interest.
	pub fn restrict(&self, prefix: &Path) -> Self {
		let mut floors: BTreeMap<Path, Rev> = BTreeMap::new();
		for (region, &floor) in &self.floors {
			let narrowed = if within(region, prefix) {
				region
			} else if within(prefix, region) {
				prefix
			} else {
				continue;
			};
			let entry = floors.entry(narrowed.clone()).or_insert(floor);
			*entry = (*entry).max(floor);
		}
		// A claim stays only while a kept floor still certifies it.
		let items = self
			.items
			.iter()
			.filter(|(claim, _)| floors.keys().any(|region| overlaps(claim, region)))
			.map(|(claim, item)| (claim.clone(), item.clone()))
			.collect();
		Self { floors, items }
	}

	/// The view restricted to the union of `regions`.
	pub fn restrict_all(&self, regions: &[Path]) -> Self {
		let mut out = Self::default();
		for region in regions {
			out.merge(&self.restrict(region));
		}
		out
	}

	/// The newest revision this view knows `claim` at, if any region covers it.
	pub fn as_of(&self, claim: &Path) -> Option<Rev> {
		self.floors
			.iter()
			.filter(|(region, _)| overlaps(claim, region))
			.map(|(_, floor)| *floor)
			.max()
	}

	/// Take `other`'s state for every claim it knows at a newer revision, then
	/// raise the floors and drop any region a wider one at least as new covers.
	pub fn merge(&mut self, other: &Self) {
		let claims: BTreeSet<Path> = self.items.keys().chain(other.items.keys()).cloned().collect();
		for claim in claims {
			if other.as_of(&claim) > self.as_of(&claim) {
				match other.items.get(&claim) {
					Some(item) => self.items.insert(claim, item.clone()),
					None => self.items.remove(&claim),
				};
			}
		}
		for (region, &floor) in &other.floors {
			let entry = self.floors.entry(region.clone()).or_insert(floor);
			*entry = (*entry).max(floor);
		}
		let floors = self.floors.clone();
		self.floors.retain(|region, floor| {
			!floors
				.iter()
				.any(|(wider, newer)| wider != region && within(region, wider) && newer >= floor)
		});
	}

	/// What a node keeps of its view once its interest is `watch`.
	///
	/// A floor outside the interest may certify a claim above both at a newer
	/// revision than any floor inside, so it stays, with what it certifies,
	/// until a floor inside catches up; dropping it would leave that claim
	/// known only at an older revision than the item it holds. Once no
	/// neighbor certifies it, pruning lets it go.
	pub fn retire(&self, watch: &Path) -> Self {
		let mut kept = self.restrict(watch);
		let newest = kept.floors.values().max().copied();
		for (region, &floor) in &self.floors {
			if Some(floor) > newest && !kept.floors.contains_key(region) {
				kept.floors.insert(region.clone(), floor);
				for (claim, item) in &self.items {
					if overlaps(claim, region) {
						kept.items.insert(claim.clone(), item.clone());
					}
				}
			}
		}
		kept
	}

	/// The changes that move a receiver from `old` to `self`: every changed or
	/// added claim with its attributes, and every removed claim with `None`.
	pub fn diff(&self, old: &Self) -> Vec<(Path, Option<A>)> {
		let mut out: Vec<(Path, Option<A>)> = self
			.items
			.iter()
			.filter(|(claim, item)| old.items.get(*claim) != Some(item))
			.map(|(claim, item)| (claim.clone(), Some(item.clone())))
			.collect();
		out.extend(
			old.items
				.keys()
				.filter(|claim| !self.items.contains_key(*claim))
				.map(|claim| (claim.clone(), None)),
		);
		out
	}

	/// Apply a batch: its changes, then its floor list, which replaces the
	/// last one.
	pub fn apply(&mut self, changes: Vec<(Path, Option<A>)>, floors: BTreeMap<Path, Rev>) {
		for (claim, item) in changes {
			match item {
				Some(item) => self.items.insert(claim, item),
				None => self.items.remove(&claim),
			};
		}
		self.floors = floors;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn p(s: &'static str) -> Path {
		Path::from_static(s)
	}

	fn view(floors: &[(&'static str, Rev)], items: &[(&'static str, u32)]) -> View<u32> {
		View {
			floors: floors.iter().map(|(r, f)| (p(r), *f)).collect(),
			items: items.iter().map(|(c, i)| (p(c), *i)).collect(),
		}
	}

	#[test]
	fn restrict_narrows_regions_to_the_interest() {
		let v = view(&[("", 5)], &[("live/a", 1), ("live", 2), ("other/x", 3)]);
		let r = v.restrict(&p("live/a"));
		assert_eq!(r, view(&[("live/a", 5)], &[("live/a", 1), ("live", 2)]));
	}

	#[test]
	fn merge_keeps_the_newer_knowledge_per_claim() {
		let mut a = view(&[("live", 3)], &[("live/x", 1)]);
		let b = view(&[("live", 4)], &[]);
		a.merge(&b);
		assert_eq!(a, view(&[("live", 4)], &[]));

		// An older view revives nothing.
		let c = view(&[("live", 2)], &[("live/x", 1)]);
		a.merge(&c);
		assert_eq!(a, view(&[("live", 4)], &[]));
	}

	#[test]
	fn merge_drops_a_region_a_wider_newer_floor_covers() {
		let mut a = view(&[("live/a", 3)], &[]);
		a.merge(&view(&[("", 4)], &[]));
		assert_eq!(a.floors, view(&[("", 4)], &[]).floors);
	}

	#[test]
	fn diff_then_apply_round_trips() {
		let old = view(&[("", 1)], &[("a", 1), ("b", 2)]);
		let new = view(&[("", 2)], &[("a", 1), ("b", 3), ("c", 4)]);
		let mut got = old.clone();
		got.apply(new.diff(&old), new.floors.clone());
		assert_eq!(got, new);

		let gone = view(&[("", 3)], &[("c", 4)]);
		got.apply(gone.diff(&new), gone.floors.clone());
		assert_eq!(got, gone);
	}
}
