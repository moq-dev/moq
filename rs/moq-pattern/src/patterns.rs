use super::{IntersectionError, InvalidPattern, Pattern};

/// A union of patterns, reduced so no member is contained by another.
///
/// This is the shape of a grant (the paths a token may publish) and of a rebased
/// pattern (see [`Pattern::rebase`]). Order is canonical, so two unions describing the
/// same reduced set compare equal.
///
/// Containment is per member: [`contains`](Self::contains) holds when one pattern in the
/// union contains the candidate. A candidate covered only jointly by several members
/// (`a/**` against `a`, `a/*`, and `a/*/**`) is refused, which keeps the check linear
/// and its answer easy to predict. A grant that means a subtree writes `a/**`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Patterns(Vec<Pattern>);

impl Patterns {
	/// The empty union, which matches nothing.
	pub fn new() -> Self {
		Self::default()
	}

	/// Add a pattern, dropping members it contains.
	///
	/// Returns `false` when a member already contains it, leaving the union unchanged.
	pub fn insert(&mut self, pattern: Pattern) -> bool {
		if self.contains(&pattern) {
			return false;
		}
		self.0.retain(|member| !pattern.contains(member));
		let at = self.0.partition_point(|member| member < &pattern);
		self.0.insert(at, pattern);
		true
	}

	/// Whether any member matches `path`.
	pub fn matches(&self, path: &str) -> bool {
		self.0.iter().any(|member| member.matches(path))
	}

	/// Whether some member contains `pattern`. See the type docs for why this is per
	/// member rather than joint.
	pub fn contains(&self, pattern: &Pattern) -> bool {
		self.0.iter().any(|member| member.contains(pattern))
	}

	/// Whether every member of `other` is [contained](Self::contains) here: `other`
	/// grants nothing this union does not. The empty union is covered by anything.
	pub fn covers(&self, other: &Self) -> bool {
		other.0.iter().all(|pattern| self.contains(pattern))
	}

	/// Whether any member overlaps `pattern`.
	pub fn overlaps(&self, pattern: &Pattern) -> bool {
		self.0.iter().any(|member| member.overlaps(pattern))
	}

	/// The paths in both unions, as one union: every member of this one intersected
	/// with every member of `other`. See [`Pattern::intersect`].
	pub fn intersect(&self, other: &Self) -> Result<Self, IntersectionError> {
		let mut out = Self::new();
		for member in &self.0 {
			for candidate in &other.0 {
				for pattern in member.intersect(candidate)? {
					out.insert(pattern);
					if out.len() > Pattern::MAX_INTERSECTIONS {
						return Err(IntersectionError::TooManyPatterns);
					}
				}
			}
		}
		Ok(out)
	}

	/// Every member rebased at `root`, as one union. See [`Pattern::rebase`].
	pub fn rebase(&self, root: &str) -> Self {
		self.0.iter().flat_map(|member| member.rebase(root)).collect()
	}

	/// Every member placed beneath `root`. See [`Pattern::rooted`].
	pub fn rooted(&self, root: &str) -> Result<Self, InvalidPattern> {
		self.0.iter().map(|member| member.rooted(root)).collect()
	}

	/// The members, in canonical order.
	pub fn iter(&self) -> std::slice::Iter<'_, Pattern> {
		self.0.iter()
	}

	/// The number of members.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether there are no members, so nothing matches.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}
}

#[cfg(feature = "serde")]
impl serde::Serialize for Patterns {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_seq(self.iter())
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Patterns {
	/// Reads a list and reduces it, so a persisted union is canonical after a round trip.
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Ok(Vec::<Pattern>::deserialize(deserializer)?.into_iter().collect())
	}
}

impl std::ops::Deref for Patterns {
	type Target = [Pattern];

	fn deref(&self) -> &[Pattern] {
		&self.0
	}
}

impl From<Pattern> for Patterns {
	fn from(pattern: Pattern) -> Self {
		Self(vec![pattern])
	}
}

impl FromIterator<Pattern> for Patterns {
	/// Reduce a union in one pass, cheapest-first.
	///
	/// [`insert`](Self::insert) is linear in the members already held, so folding a
	/// peer-supplied list through it costs O(n²). Sorting by
	/// [`specificity`](Pattern::specificity) first makes the containment scan cheap:
	/// a pattern can only be contained by one of strictly lower specificity, so every
	/// member of its own tier is skipped without a comparison, and a later pattern can
	/// never contain an earlier one, so no member is ever removed.
	fn from_iter<I: IntoIterator<Item = Pattern>>(iter: I) -> Self {
		let mut sorted: Vec<Pattern> = iter.into_iter().collect();
		sorted.sort_unstable_by(|a, b| {
			a.specificity()
				.cmp(&b.specificity())
				.then_with(|| a.as_str().cmp(b.as_str()))
		});
		sorted.dedup_by(|a, b| a == b);

		// Grow with what survives: reserving for the input would keep capacity for every
		// pattern a broad member discards, for as long as the union lives.
		let mut out = Patterns::new();
		let mut tier_start = 0;
		let mut tier = None;
		for pattern in sorted {
			let specificity = pattern.specificity();
			if tier != Some(specificity) {
				tier = Some(specificity);
				tier_start = out.0.len();
			}
			if out.0[..tier_start].iter().any(|member| member.contains(&pattern)) {
				continue;
			}
			out.0.push(pattern);
		}

		// The wire order is canonical text, whatever order reduction visited.
		out.0.sort_unstable();
		out
	}
}

impl Extend<Pattern> for Patterns {
	fn extend<I: IntoIterator<Item = Pattern>>(&mut self, iter: I) {
		for pattern in iter {
			self.insert(pattern);
		}
	}
}

impl IntoIterator for Patterns {
	type Item = Pattern;
	type IntoIter = std::vec::IntoIter<Pattern>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Patterns {
	type Item = &'a Pattern;
	type IntoIter = std::slice::Iter<'a, Pattern>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.iter()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pattern(text: &str) -> Pattern {
		text.parse().unwrap()
	}

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|t| pattern(t)).collect()
	}

	#[test]
	fn insert_reduces_by_containment() {
		let mut set = Patterns::new();
		assert!(set.insert(pattern("a/b")));
		assert!(set.insert(pattern("a/c")));
		assert!(!set.insert(pattern("a/b")));
		assert!(set.insert(pattern("a/*")));
		assert_eq!(set, patterns(&["a/*"]));
		assert!(!set.insert(pattern("a/d")));
		assert!(set.insert(pattern("**/x")));
		assert_eq!(set.len(), 2);
		assert!(set.insert(pattern("**")));
		assert_eq!(set, patterns(&["**"]));
	}

	#[test]
	fn equality_is_set_equality() {
		assert_eq!(patterns(&["a", "b"]), patterns(&["b", "a"]));
		assert_eq!(patterns(&["a", "a/**", "b"]), patterns(&["b", "a/**"]));
		assert_ne!(patterns(&["a"]), patterns(&["a", "b"]));
	}

	#[test]
	fn joint_coverage_is_not_containment() {
		let set = patterns(&["a", "a/*", "a/*/**"]);
		assert!(!set.contains(&pattern("a/**")));
		assert!(set.contains(&pattern("a/x/**")));
		assert!(set.covers(&patterns(&["a", "a/x/y"])));
		assert!(!set.covers(&patterns(&["a", "b"])));
		assert!(set.covers(&Patterns::new()));
		assert!(!Patterns::new().covers(&set));
	}

	#[test]
	fn matches_and_overlaps_any_member() {
		let set = patterns(&["a/**", "**/c"]);
		assert!(set.matches("a"));
		assert!(set.matches("x/c"));
		assert!(!set.matches("x/y"));
		assert!(set.overlaps(&pattern("*/c")));
		assert!(!set.overlaps(&pattern("b/d")));
		assert!(!Patterns::new().matches(""));
	}

	#[cfg(feature = "serde")]
	#[test]
	fn serde_round_trips_reduced() {
		let set: Patterns = serde_json::from_str(r#"["a/b", "a/*", "**/c"]"#).unwrap();
		assert_eq!(serde_json::to_string(&set).unwrap(), r#"["**/c","a/*"]"#);
	}

	#[test]
	fn from_iter_matches_repeated_insert() {
		// The bulk path must produce exactly the union insert would, including
		// duplicates and patterns whose tiers differ. A tiny LCG keeps it deterministic.
		let atoms = [
			"a", "b", "c", "a/b", "a/c", "*", "a/*", "*/a", "a*", "*a", "a*b", "a/**", "**/a", "**", "",
		];
		let mut seed = 0x9e37_79b9_7f4a_7c15u64;
		for _ in 0..500 {
			let mut picked = Vec::new();
			for _ in 0..8 {
				seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
				picked.push(pattern(atoms[(seed >> 33) as usize % atoms.len()]));
			}
			let mut expected = Patterns::new();
			for candidate in &picked {
				expected.insert(candidate.clone());
			}
			assert_eq!(picked.iter().cloned().collect::<Patterns>(), expected, "{picked:?}");
		}
	}

	#[test]
	fn many_incomparable_patterns_reduce() {
		// Same tier, so neither contains the other and the reduction must keep all of them.
		let members: Vec<Pattern> = (0..2000).map(|i| pattern(&format!("room/{i}"))).collect();
		let set: Patterns = members.iter().cloned().collect();
		assert_eq!(set.len(), members.len());
		assert!(members.iter().all(|member| set.contains(member)));
	}

	#[test]
	fn intersect_is_pairwise() {
		let grant = patterns(&["a/*", "b/**"]);
		let claim = patterns(&["*/c", "b/d/**"]);
		assert_eq!(grant.intersect(&claim).unwrap(), patterns(&["a/c", "b/c", "b/d/**"]));
		assert!(grant.intersect(&patterns(&["x"])).unwrap().is_empty());
		assert!(Patterns::new().intersect(&grant).unwrap().is_empty());
	}

	#[test]
	fn rebase_and_rooted_round_trip() {
		let set = patterns(&["**/a", "b/**"]);
		assert_eq!(set.rebase("a"), patterns(&["", "**/a"]));
		assert_eq!(set.rebase("b/c"), patterns(&["**"]));
		assert_eq!(set.rebase("x"), patterns(&["**/a"]));
		assert_eq!(set.rooted("r").unwrap(), patterns(&["r/**/a", "r/b/**"]));
		assert!(set.rooted("*").is_err());
	}
}
