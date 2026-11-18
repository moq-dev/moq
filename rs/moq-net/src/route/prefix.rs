//! Lookups by prefix in the route layer's path-keyed maps.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::{Bound, RangeBounds},
};

use crate::Path;

/// Whether `path` is `prefix` or lies below it, matching whole segments.
pub(crate) fn within(path: &Path, prefix: &Path) -> bool {
	path.has_prefix(prefix)
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

/// Whether `path` lies within any of `regions`: one lookup per segment.
pub(crate) fn within_any(path: &Path, regions: &BTreeSet<Path>) -> bool {
	heads(path).any(|head| regions.contains(head))
}

/// `path` and every prefix above it, the root first, as the strings a `Path`-keyed
/// map is looked up by.
pub(crate) fn heads(path: &Path) -> impl Iterator<Item = &str> {
	let s = path.as_str();
	let ends = path.parts().scan(0, |end, part| {
		*end += part.len() + usize::from(*end > 0);
		Some(*end)
	});
	std::iter::once(0).chain(ends).map(move |end| &s[..end])
}
