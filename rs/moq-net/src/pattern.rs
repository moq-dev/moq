use crate::{AsPath, Path};

/// A pattern of paths a publisher could serve on demand: a prefix and a
/// suffix, either possibly empty.
///
/// A path matches when it starts with the prefix and what remains ends with
/// the suffix, both segment-aware, so the two halves never overlap. Both
/// halves empty matches every path. This is the shape a dynamic advertisement
/// carries (draft-lcurley-moq-lite, Dynamic Stream); it is a capability, not
/// an inventory, and never implies that a matching path exists.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Pattern<'a> {
	pub prefix: Path<'a>,
	pub suffix: Path<'a>,
}

pub type PatternOwned = Pattern<'static>;

impl<'a> Pattern<'a> {
	pub fn new(prefix: impl Into<Path<'a>>, suffix: impl Into<Path<'a>>) -> Self {
		Self {
			prefix: prefix.into(),
			suffix: suffix.into(),
		}
	}

	/// The pattern that matches every path.
	pub fn any() -> Self {
		Self {
			prefix: Path::empty(),
			suffix: Path::empty(),
		}
	}

	/// Whether `path` matches this pattern: it has the prefix, and what
	/// remains ends with the suffix.
	///
	/// # Examples
	/// ```
	/// use moq_net::{Path, Pattern};
	///
	/// let pattern = Pattern::new("", "transcode.pro");
	/// assert!(pattern.matches("room/cam/transcode.pro"));
	/// assert!(!pattern.matches("room/cam"));
	///
	/// // The halves must not overlap.
	/// let pattern = Pattern::new("foo", "foo");
	/// assert!(!pattern.matches("foo"));
	/// assert!(pattern.matches("foo/foo"));
	/// ```
	pub fn matches(&self, path: impl AsPath) -> bool {
		let path = path.as_path();
		match path.strip_prefix(&self.prefix) {
			Some(rest) => rest.has_suffix(&self.suffix),
			None => false,
		}
	}

	/// Whether some path under `prefix` could match this pattern, which is
	/// what decides if the pattern belongs on a stream requesting `prefix`.
	///
	/// True when either prefix extends the other; the suffix claims nothing
	/// about where a path begins, so it never disqualifies.
	pub fn overlaps(&self, prefix: impl AsPath) -> bool {
		let prefix = prefix.as_path();
		self.prefix.has_prefix(&prefix) || prefix.has_prefix(&self.prefix)
	}

	/// How specific this pattern is: the literal segments it pins, prefix
	/// plus suffix. Only the most specific matching tier is consulted during
	/// resolution, so equal values pool and a higher value shadows.
	pub fn specificity(&self) -> usize {
		self.prefix.parts().count() + self.suffix.parts().count()
	}

	pub fn to_owned(&self) -> PatternOwned {
		Pattern {
			prefix: self.prefix.to_owned(),
			suffix: self.suffix.to_owned(),
		}
	}

	pub fn borrow(&'a self) -> Pattern<'a> {
		Pattern {
			prefix: self.prefix.borrow(),
			suffix: self.suffix.borrow(),
		}
	}
}

impl std::fmt::Display for Pattern<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}/**/{}", self.prefix, self.suffix)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_matches() {
		let pattern = Pattern::new("room", "transcode.pro");
		assert!(pattern.matches("room/cam/transcode.pro"));
		assert!(pattern.matches("room/a/b/transcode.pro"));
		assert!(!pattern.matches("room/transcode.pro2"));
		assert!(!pattern.matches("lobby/cam/transcode.pro"));

		// Adjacent halves: the path is exactly prefix/suffix.
		assert!(pattern.matches("room/transcode.pro"));

		// Segment boundaries hold on both halves.
		assert!(!Pattern::new("roo", "").matches("room/cam"));
		assert!(!Pattern::new("", "am").matches("room/cam"));

		// The catch-all matches everything, including the empty path.
		assert!(Pattern::any().matches("anything/at/all"));
		assert!(Pattern::any().matches(""));

		// The halves must not overlap.
		let pattern = Pattern::new("foo", "foo");
		assert!(!pattern.matches("foo"));
		assert!(pattern.matches("foo/foo"));
	}

	#[test]
	fn test_overlaps() {
		let pattern = Pattern::new("room", "transcode.pro");
		assert!(pattern.overlaps(""));
		assert!(pattern.overlaps("room"));
		assert!(pattern.overlaps("room/cam"));
		assert!(!pattern.overlaps("lobby"));

		// A suffix-only pattern overlaps every prefix.
		assert!(Pattern::new("", "transcode.pro").overlaps("deeply/nested/prefix"));
	}

	#[test]
	fn test_specificity() {
		assert_eq!(Pattern::any().specificity(), 0);
		assert_eq!(Pattern::new("", "transcode.pro").specificity(), 1);
		assert_eq!(Pattern::new("room/cam", "a/b").specificity(), 4);
	}
}
