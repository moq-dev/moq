//! An origin's claims as one node knows them, certified per region by floors.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::{Bound, RangeBounds},
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

	/// The newest revision this view knows `claim` at, if a region certifies it.
	#[cfg(test)]
	pub fn as_of(&self, claim: &Path) -> Option<Rev> {
		above(&self.floors, claim).map(|(_, &floor)| floor).max()
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

/// All descendants of a prefix. Whole segments lie in `prefix/`..`prefix0`;
/// the root excludes only itself. Both map and set indexes use these bounds.
struct Descendants {
	lo: String,
	hi: Option<String>,
	root: bool,
}

impl Descendants {
	fn new(prefix: &Path) -> Self {
		Self {
			lo: if prefix.is_empty() {
				String::new()
			} else {
				format!("{prefix}/")
			},
			hi: (!prefix.is_empty()).then(|| format!("{prefix}0")),
			root: prefix.is_empty(),
		}
	}
}

impl RangeBounds<str> for Descendants {
	fn start_bound(&self) -> Bound<&str> {
		if self.root {
			Bound::Excluded(&self.lo)
		} else {
			Bound::Included(&self.lo)
		}
	}

	fn end_bound(&self) -> Bound<&str> {
		self.hi.as_deref().map_or(Bound::Unbounded, Bound::Excluded)
	}
}

/// The entries of `map` strictly below `prefix`, as one range.
pub(crate) fn below<'a, V>(map: &'a BTreeMap<Path, V>, prefix: &Path) -> impl Iterator<Item = (&'a Path, &'a V)> {
	map.range::<str, _>(Descendants::new(prefix))
}

/// The paths in `set` at or below `prefix`, without walking its siblings.
pub(crate) fn within_set<'a>(set: &'a BTreeSet<Path>, prefix: &Path) -> impl Iterator<Item = &'a Path> {
	set.get(prefix)
		.into_iter()
		.chain(set.range::<str, _>(Descendants::new(prefix)))
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

/// `path` and every prefix above it, the root first, as the strings a `Path`-keyed
/// map is looked up by.
fn heads(path: &Path) -> impl Iterator<Item = &str> {
	let s = path.as_str();
	let ends = s.match_indices('/').map(|(end, _)| end);
	(std::iter::once(0).chain(ends).chain((!s.is_empty()).then_some(s.len()))).map(move |end| &s[..end])
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
	fn a_floor_certifies_only_what_lies_within_it() {
		let v = view(&[("live/a", 5)], &[("live/a", 1), ("live/a/x", 2), ("live", 3)]);
		assert_eq!(v.as_of(&p("live/a")), Some(5));
		assert_eq!(v.as_of(&p("live/a/x")), Some(5));
		assert_eq!(v.as_of(&p("live")), None);
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
		}
	}
}
