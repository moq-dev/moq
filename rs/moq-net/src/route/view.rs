//! An origin's claims as one node knows them, certified per region by floors.

use std::collections::{BTreeMap, BTreeSet};

use crate::Path;

use super::Rev;

/// Whether `path` is `prefix` or lies below it, matching whole segments.
pub(crate) fn within(path: &Path, prefix: &Path) -> bool {
	path.has_prefix(prefix)
}

/// What a node knows of one origin's claims.
///
/// Each region's floor certifies that the claims within it are exactly the
/// origin's claims there as of that revision, as a stream answering that region
/// presents them: every claim inside the region at its own path, and the most
/// specific one above it at the region itself (see [`View::present`]). A claim
/// within no floor is uncertified: some stream carries it, and nothing proves it
/// current.
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
	/// The view as a stream answering `prefix` presents it.
	///
	/// Every claim inside the prefix keeps its path, and of the claims above it only
	/// the most specific remains, presented at the prefix itself: a receiver can tell
	/// apart no claims that cover the whole prefix alike. A region meeting the prefix
	/// is narrowed to whichever of the two lies within the other.
	///
	/// The newest floor over the prefix certifies what covers it there: a claim
	/// between the two, else the one that floor presents at its own region, else
	/// none. A claim above that floor is older knowledge, or uncertified, so it is
	/// not presented.
	pub fn present(&self, prefix: &Path) -> Self {
		let mut out = Self {
			floors: narrow(&self.floors, prefix),
			items: BTreeMap::new(),
		};
		let mut covering: Option<(&Path, &A)> = None;
		for (claim, item) in &self.items {
			if within(claim, prefix) {
				out.items.insert(claim.clone(), item.clone());
			}
			if within(prefix, claim) && covering.is_none_or(|(best, _)| claim.len() > best.len()) {
				covering = Some((claim, item));
			}
		}
		if let Some(&floor) = out.floors.get(prefix) {
			let certifier = self
				.floors
				.iter()
				.filter(|&(region, &rev)| rev == floor && within(prefix, region))
				.map(|(region, _)| region)
				.max_by_key(|region| region.len());
			if certifier.is_some_and(|certifier| covering.is_some_and(|(claim, _)| !within(claim, certifier))) {
				covering = None;
			}
		}
		if let Some((_, item)) = covering {
			out.items.entry(prefix.clone()).or_insert_with(|| item.clone());
		}
		out
	}

	/// The view presented to the union of `regions`.
	pub fn present_all(&self, regions: &[Path]) -> Self {
		let mut out = Self::default();
		for region in regions {
			let presented = self.present(region);
			out.items.extend(presented.items);
			for (region, floor) in presented.floors {
				let entry = out.floors.entry(region).or_insert(floor);
				*entry = (*entry).max(floor);
			}
		}
		out
	}

	/// What the view certifies within `prefix`: the view as presented there, less
	/// the claims no kept floor certifies.
	pub fn restrict(&self, prefix: &Path) -> Self {
		let mut out = self.present(prefix);
		let floors = &out.floors;
		out.items
			.retain(|claim, _| floors.keys().any(|region| within(claim, region)));
		out
	}

	/// The view restricted to the union of `regions`.
	pub fn restrict_all(&self, regions: &[Path]) -> Self {
		let mut out = Self::default();
		for region in regions {
			out.merge(&self.restrict(region));
		}
		out
	}

	/// The newest revision this view knows `claim` at, if a region certifies it.
	pub fn as_of(&self, claim: &Path) -> Option<Rev> {
		self.floors
			.iter()
			.filter(|(region, _)| within(claim, region))
			.map(|(_, floor)| *floor)
			.max()
	}

	/// The claims no floor certifies.
	pub fn uncertified(&self) -> impl Iterator<Item = (&Path, &A)> {
		self.items.iter().filter(|(claim, _)| self.as_of(claim).is_none())
	}

	/// The single revision the view certifies all of `prefix` at, if one floor
	/// covers it and nothing within it is known at another revision.
	pub fn floor(&self, prefix: &Path) -> Option<Rev> {
		match self.floors.iter().next() {
			Some((region, &rev)) if self.floors.len() == 1 && region == prefix => Some(rev),
			_ => None,
		}
	}

	/// Take `other`'s state for every claim it knows at a newer revision, then
	/// raise the floors and drop any region a wider one at least as new covers.
	/// An uncertified claim of `other` is known at no revision, so it is not taken.
	pub fn merge(&mut self, other: &Self) {
		let claims: BTreeSet<Path> = self.items.keys().chain(other.items.keys()).cloned().collect();
		for claim in claims {
			let (theirs, ours) = (other.as_of(&claim), self.as_of(&claim));
			// At one revision, a claim exactly on a wider region outranks the claim
			// above it that a floor on this path presented here.
			let exact = theirs == ours && self.floors.contains_key(&claim) && !other.floors.contains_key(&claim);
			if theirs > ours || exact {
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
					if within(claim, region) {
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

	/// Apply a batch: its changes, then its floors, which replace the last.
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

/// `floors` narrowed to `prefix`: a region meeting it becomes whichever of the two
/// lies within the other, keeping the newest floor where two narrow alike.
fn narrow(floors: &BTreeMap<Path, Rev>, prefix: &Path) -> BTreeMap<Path, Rev> {
	let mut out: BTreeMap<Path, Rev> = BTreeMap::new();
	for (region, &floor) in floors {
		let narrowed = if within(region, prefix) {
			region
		} else if within(prefix, region) {
			prefix
		} else {
			continue;
		};
		let entry = out.entry(narrowed.clone()).or_insert(floor);
		*entry = (*entry).max(floor);
	}
	out
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
	fn present_narrows_regions_and_moves_the_covering_claim_onto_the_interest() {
		let v = view(&[("", 5)], &[("live/a/x", 1), ("live", 2), ("other/x", 3)]);
		let r = v.present(&p("live/a"));
		assert_eq!(r, view(&[("live/a", 5)], &[("live/a/x", 1), ("live/a", 2)]));
		assert_eq!(r.floor(&p("live/a")), Some(5));
	}

	#[test]
	fn present_keeps_only_the_most_specific_covering_claim() {
		let v = view(&[("", 5)], &[("", 9), ("live", 10), ("live/a", 1)]);
		assert_eq!(v.present(&p("live/a/x")), view(&[("live/a/x", 5)], &[("live/a/x", 1)]));
		// A claim at the interest itself is the most specific there is.
		let v = view(&[("", 5)], &[("live", 10), ("live/a", 1)]);
		assert_eq!(v.present(&p("live/a")), view(&[("live/a", 5)], &[("live/a", 1)]));
	}

	#[test]
	fn restrict_moves_a_certified_covering_claim_and_drops_an_uncertified_one() {
		let v = view(&[("", 5)], &[("live/a/x", 1), ("live", 2)]);
		assert_eq!(
			v.restrict(&p("live/a")),
			view(&[("live/a", 5)], &[("live/a", 2), ("live/a/x", 1)])
		);
		// The floor proves no claim covers its region, so the claim above is stale.
		let v = view(&[("live/a", 5)], &[("live/a/x", 1), ("live", 2)]);
		assert_eq!(v.restrict(&p("live/a/y")), view(&[("live/a/y", 5)], &[]));
	}

	#[test]
	fn a_wider_floor_at_one_revision_replaces_a_claim_moved_onto_a_narrower_one() {
		let mut a = view(&[("live/b/z", 5)], &[("live/b/z", 1)]);
		a.merge(&view(&[("live/b", 5)], &[("live/b", 1)]));
		assert_eq!(a, view(&[("live/b", 5)], &[("live/b", 1)]));
	}

	#[test]
	fn a_floor_certifies_only_what_lies_within_it() {
		let v = view(&[("live/a", 5)], &[("live/a", 1), ("live/a/x", 2), ("live", 3)]);
		assert_eq!(v.as_of(&p("live/a")), Some(5));
		assert_eq!(v.as_of(&p("live/a/x")), Some(5));
		assert_eq!(v.as_of(&p("live")), None);
		assert_eq!(
			v.uncertified().map(|(claim, _)| claim.as_str()).collect::<Vec<_>>(),
			["live"]
		);
		// Two floors certify no single revision.
		let v = view(&[("live", 5), ("live/a", 6)], &[]);
		assert_eq!(v.floor(&p("live")), None);
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
