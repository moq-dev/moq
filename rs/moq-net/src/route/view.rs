//! An origin's claims as one node knows them, certified per region by floors.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::Bound,
};

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
	/// the most specific remains, presented at the prefix itself (see [`covering`]): a
	/// receiver can tell apart no claims that cover the whole prefix alike. A region
	/// meeting the prefix is narrowed to whichever of the two lies within the other.
	pub fn present(&self, prefix: &Path) -> Self {
		let mut items: BTreeMap<Path, A> = below(&self.items, prefix)
			.map(|(claim, item)| (claim.clone(), item.clone()))
			.collect();
		if let Some(item) = covering(&self.floors, prefix, |claim| self.items.get(claim)) {
			items.insert(prefix.clone(), item.clone());
		}
		Self {
			floors: narrow(&self.floors, prefix),
			items,
		}
	}

	/// The view presented to the union of `regions`.
	#[cfg(test)]
	pub fn present_all(&self, regions: &[Path]) -> Self {
		let mut out = Self::default();
		out.represent(self, regions, self.items.keys());
		out
	}

	/// Bring this view of `raw`, presented to the union of `regions`, up to date once
	/// `claims` of `raw` changed and its floors were replaced.
	///
	/// A claim strictly below a region keeps its path, and each region is shown what
	/// covers it (see [`covering`]), which is the claim exactly there when one is. Only
	/// the regions' covering claims and the named claims can change, so the work
	/// follows those rather than the view's size.
	pub fn represent<'a>(&mut self, raw: &Self, regions: &[Path], claims: impl IntoIterator<Item = &'a Path>) {
		for claim in claims {
			// Shown at its region by the covering claim below.
			if regions.contains(claim) {
				continue;
			}
			let below = regions.iter().any(|region| claim != region && within(claim, region));
			match raw.items.get(claim).filter(|_| below) {
				Some(item) => self.items.insert(claim.clone(), item.clone()),
				None => self.items.remove(claim),
			};
		}
		for region in regions {
			match covering(&raw.floors, region, |claim| raw.items.get(claim)) {
				Some(item) => self.items.insert(region.clone(), item.clone()),
				None => self.items.remove(region),
			};
		}
		self.floors.clear();
		for region in regions {
			for (region, floor) in narrow(&raw.floors, region) {
				let entry = self.floors.entry(region).or_insert(floor);
				*entry = (*entry).max(floor);
			}
		}
	}

	/// What the view certifies within `prefix`: the view as presented there, less
	/// the claims no kept floor certifies.
	pub fn restrict(&self, prefix: &Path) -> Self {
		let mut out = self.present(prefix);
		let floors = &out.floors;
		out.items.retain(|claim, _| above(floors, claim).next().is_some());
		out
	}

	/// The view restricted to the union of `regions`, as merging each region's
	/// restriction would leave it, in one pass.
	///
	/// A region within another adds nothing the wider one's restriction lacks: the
	/// floors over it narrow to the wider region, where they certify the claim at the
	/// region itself at its own path. So only the widest regions count, and those are
	/// disjoint, so their restrictions merge as a union. A floor a wider one at least
	/// as new covers then goes, as [`Self::merge`] drops it.
	pub fn restrict_all(&self, regions: &BTreeSet<Path>) -> Self {
		let mut out = Self::default();
		for region in regions {
			let wider = heads(region).filter(|&head| head != region.as_str());
			if wider.into_iter().any(|head| regions.contains(head)) {
				continue;
			}
			let Self { floors, items } = self.restrict(region);
			out.floors.extend(floors);
			out.items.extend(items);
		}
		settle(&mut out.floors);
		out
	}

	/// The newest revision this view knows `claim` at, if a region certifies it.
	pub fn as_of(&self, claim: &Path) -> Option<Rev> {
		above(&self.floors, claim).map(|(_, &floor)| floor).max()
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
	/// Returns the claims whose state changed.
	pub fn merge(&mut self, other: &Self) -> Vec<Path> {
		let claims: BTreeSet<Path> = self.items.keys().chain(other.items.keys()).cloned().collect();
		self.merge_claims(other, &claims)
	}

	/// Every floor of `other` lies within a newer one here, so `other` knows no claim
	/// at a newer revision and merging it changes nothing.
	fn outdates(&self, other: &Self) -> bool {
		(other.floors.iter()).all(|(region, &floor)| above(&self.floors, region).any(|(_, &newer)| newer > floor))
	}

	/// [`Self::merge`], considering only `claims`: the same once they include every
	/// claim whose state differs between the two views, since a claim held alike is
	/// never taken.
	pub fn merge_claims<'a>(&mut self, other: &Self, claims: impl IntoIterator<Item = &'a Path>) -> Vec<Path> {
		// A lagging view costs nothing to merge, however far it lags.
		if self.outdates(other) {
			return Vec::new();
		}
		let mut changed = Vec::new();
		for claim in claims {
			let (theirs, ours) = (other.as_of(claim), self.as_of(claim));
			// At one revision, a claim exactly on a wider region outranks the claim
			// above it that a floor on this path presented here.
			let exact = theirs == ours && self.floors.contains_key(claim) && !other.floors.contains_key(claim);
			if (theirs > ours || exact) && self.items.get(claim) != other.items.get(claim) {
				changed.push(claim.clone());
			}
		}
		for claim in &changed {
			match other.items.get(claim) {
				Some(item) => self.items.insert(claim.clone(), item.clone()),
				None => self.items.remove(claim),
			};
		}
		for (region, &floor) in &other.floors {
			let entry = self.floors.entry(region.clone()).or_insert(floor);
			*entry = (*entry).max(floor);
		}
		settle(&mut self.floors);
		changed
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
				let exact = self.items.get_key_value(region);
				for (claim, item) in exact.into_iter().chain(below(&self.items, region)) {
					kept.items.insert(claim.clone(), item.clone());
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

/// The entries of `map` strictly below `prefix`, as one range: a path lies below a
/// non-empty prefix exactly when it starts with the prefix and a `/`.
pub(crate) fn below<'a, V>(map: &'a BTreeMap<Path, V>, prefix: &Path) -> impl Iterator<Item = (&'a Path, &'a V)> {
	let (lo, hi) = match prefix.is_empty() {
		true => (String::new(), None),
		false => (format!("{prefix}/"), Some(format!("{prefix}0"))),
	};
	let lo = match prefix.is_empty() {
		true => Bound::Excluded(lo.as_str()),
		false => Bound::Included(lo.as_str()),
	};
	let hi = hi.as_deref().map_or(Bound::Unbounded, Bound::Excluded);
	map.range::<str, _>((lo, hi))
}

/// `path` and every prefix above it, the root first.
pub(crate) fn ancestors(path: &Path) -> impl DoubleEndedIterator<Item = Path> + '_ {
	(0..=path.parts().count()).filter_map(|n| path.head(n))
}

/// The claim a stream answering `prefix` is shown at `prefix` itself, looked up by
/// `item`: one exactly there, else the most specific claim above it.
///
/// The newest floor over the prefix certifies what covers it there: a claim between
/// the two, else the one that floor presents at its own region, else none. A claim
/// above that floor is older knowledge, or uncertified, so it is not presented.
pub(crate) fn covering<'a, A>(
	floors: &BTreeMap<Path, Rev>,
	prefix: &Path,
	item: impl Fn(&Path) -> Option<&'a A>,
) -> Option<&'a A> {
	let (claim, found) = ancestors(prefix)
		.rev()
		.find_map(|claim| item(&claim).map(|found| (claim, found)))?;
	if claim == *prefix {
		return Some(found);
	}
	let Some(floor) = above(floors, prefix).map(|(_, &rev)| rev).max() else {
		return Some(found);
	};
	// The most specific of the newest, since the root comes first.
	let certifier = above(floors, prefix)
		.filter(|&(_, &rev)| rev == floor)
		.map(|(region, _)| region)
		.last();
	match certifier.is_some_and(|certifier| !within(&claim, certifier)) {
		true => None,
		false => Some(found),
	}
}

/// `floors` narrowed to `prefix`: a region meeting it becomes whichever of the two
/// lies within the other, keeping the newest floor where two narrow alike.
pub(crate) fn narrow(floors: &BTreeMap<Path, Rev>, prefix: &Path) -> BTreeMap<Path, Rev> {
	let mut out: BTreeMap<Path, Rev> = (below(floors, prefix))
		.map(|(region, &floor)| (region.clone(), floor))
		.collect();
	if let Some(floor) = above(floors, prefix).map(|(_, &rev)| rev).max() {
		out.insert(prefix.clone(), floor);
	}
	out
}

/// The entries of `map` at `path` or above it, the root first: one lookup per
/// segment, however many entries the map holds.
pub(crate) fn above<'a, V>(map: &'a BTreeMap<Path, V>, path: &'a Path) -> impl Iterator<Item = (&'a Path, &'a V)> {
	heads(path).filter_map(|head| map.get_key_value(head))
}

/// Whether `path` lies within any of `regions`: one lookup per segment.
pub(crate) fn within_any(path: &Path, regions: &BTreeSet<Path>) -> bool {
	heads(path).any(|head| regions.contains(head))
}

/// `path` and every prefix above it, the root first, as the strings a `Path`-keyed
/// map is looked up by.
fn heads(path: &Path) -> impl Iterator<Item = &str> {
	let s = path.as_str();
	let ends = s.match_indices('/').map(|(end, _)| end);
	(std::iter::once(0).chain(ends).chain((!s.is_empty()).then_some(s.len()))).map(move |end| &s[..end])
}

/// Drop each region a wider one at least as new covers: what it certifies, the
/// wider one already does.
fn settle(floors: &mut BTreeMap<Path, Rev>) {
	let covered: Vec<Path> = (floors.iter())
		.filter(|&(region, &floor)| above(floors, region).any(|(wider, &newer)| wider != region && newer >= floor))
		.map(|(region, _)| region.clone())
		.collect();
	for region in covered {
		floors.remove(&region);
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
	fn retire_keeps_an_outside_floor_only_while_it_is_newer_than_every_floor_inside() {
		let v = view(
			&[("live", 3), ("other/a", 5), ("other/b", 3), ("other/c", 2)],
			&[("live/x", 1), ("other/a/x", 2), ("other/b/x", 3), ("other/c/x", 4)],
		);
		assert_eq!(
			v.retire(&p("live")),
			view(&[("live", 3), ("other/a", 5)], &[("live/x", 1), ("other/a/x", 2)])
		);
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

	#[test]
	fn restrict_all_ignores_a_region_within_another() {
		// A newer floor over `live` but not `live/a`, and an older one exactly there:
		// restricting to `live/a` alone presents `live`'s claim at `live/a`, which the
		// wider region knows is no claim of its own.
		let v = view(&[("live", 5), ("live/a", 3)], &[("live", 1)]);
		let wide = view(&[("live", 5)], &[("live", 1)]);
		assert_eq!(v.restrict_all(&BTreeSet::from([p("live/a"), p("live")])), wide);
	}

	/// Random views over paths that share segments and string prefixes (`a` and `ab`).
	#[test]
	fn lookups_match_a_scan_of_every_floor() {
		const PATHS: [&str; 9] = ["", "a", "a/b", "a/b/c", "a/c", "ab", "ab/c", "b", "b/a"];
		let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
		let mut next = |below: u64| {
			seed ^= seed << 13;
			seed ^= seed >> 7;
			seed ^= seed << 17;
			seed % below
		};
		for _ in 0..2_000 {
			let mut v = View::<u32>::default();
			for path in PATHS {
				if next(3) == 0 {
					v.floors.insert(p(path), next(4));
				}
				if next(2) == 0 {
					v.items.insert(p(path), next(3) as u32);
				}
			}
			for path in PATHS.map(p) {
				let scan = v.floors.iter().filter(|(region, _)| within(&path, region));
				assert_eq!(v.as_of(&path), scan.map(|(_, &floor)| floor).max());
				let mut narrowed = BTreeMap::new();
				for (region, &floor) in &v.floors {
					let key = match (within(region, &path), within(&path, region)) {
						(true, _) => region.clone(),
						(_, true) => path.clone(),
						_ => continue,
					};
					let entry = narrowed.entry(key).or_insert(floor);
					*entry = (*entry).max(floor);
				}
				assert_eq!(narrow(&v.floors, &path), narrowed);
			}
			let regions: BTreeSet<Path> = PATHS.iter().filter(|_| next(3) == 0).map(|path| p(path)).collect();
			let widest: Vec<&Path> = (regions.iter())
				.filter(|&region| !regions.iter().any(|wider| wider != region && within(region, wider)))
				.collect();
			let mut folded = View::default();
			for region in widest.iter().rev() {
				folded.merge(&v.restrict(region));
			}
			assert_eq!(v.restrict_all(&regions), folded);
		}
	}
}
