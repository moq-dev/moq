use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::parts::{is_segment, parts};
use crate::{InvalidPattern, Pattern, Patterns};

/// Why a string is not a valid [`Prefix`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidPrefix {
	/// The text holds a `*`. A grant is a set of prefixes, each covering its whole subtree,
	/// so it takes no wildcard: `foo` already covers what `foo/**` meant.
	Wildcard(String),
	/// The text is not a path: an empty segment, a malformed escape, or too many segments.
	Path(InvalidPattern),
}

impl fmt::Display for InvalidPrefix {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Wildcard(text) if text == "**" => {
				write!(f, "`**` is a wildcard: grants are prefixes, and the empty prefix covers every path")
			}
			Self::Wildcard(text) => match text.strip_suffix("/**").filter(|head| !head.contains('*')) {
				Some(head) => write!(
					f,
					"`{text}` is a wildcard: grants are prefixes, and `{head}` covers `{head}` and every path beneath it"
				),
				None => write!(
					f,
					"`{text}` holds a wildcard: grants are prefixes, each covering one path and every path beneath it"
				),
			},
			Self::Path(err) => write!(f, "invalid prefix: {err}"),
		}
	}
}

impl std::error::Error for InvalidPrefix {}

impl From<InvalidPattern> for InvalidPrefix {
	fn from(err: InvalidPattern) -> Self {
		Self::Path(err)
	}
}

/// One member of a grant: a path, covering itself and every path beneath it.
///
/// The empty prefix covers every path. A prefix is spelled like a path: `/`-separated
/// segments, a `\` escaping the `\` or `/` after it, at most [`Pattern::MAX_SEGMENTS`]
/// segments, and no leading, trailing, or doubled `/`. It never holds a `*`: parsing one
/// fails loudly with [`InvalidPrefix::Wildcard`], since a grant never names a pattern.
///
/// Ordered segment by segment, so a prefix sorts right before every path beneath it.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Prefix {
	text: String,
}

impl Prefix {
	/// The empty prefix, covering every path.
	pub fn all() -> Self {
		Self::default()
	}

	/// The canonical text.
	pub fn as_str(&self) -> &str {
		&self.text
	}

	/// Whether this is the empty prefix, covering every path.
	pub fn is_empty(&self) -> bool {
		self.text.is_empty()
	}

	/// Whether `path`, spelled as a broadcast path, is this prefix or beneath it.
	pub fn matches(&self, path: &str) -> bool {
		beneath(path, &self.text)
	}

	/// Whether this prefix covers some path at or beneath the canonical `path`: one lies
	/// beneath the other.
	pub fn overlaps(&self, path: &str) -> bool {
		beneath(path, &self.text) || beneath(&self.text, path)
	}

	/// This prefix placed beneath the path `root`, normalized like a broadcast path (slashes
	/// trimmed and collapsed). Fails when `root` holds a wildcard or a malformed escape, or
	/// the result has too many segments.
	pub fn rooted(&self, root: &str) -> Result<Self, InvalidPrefix> {
		let root: Prefix = normalize(root).parse()?;
		match (root.is_empty(), self.is_empty()) {
			(true, _) => Ok(self.clone()),
			(false, true) => Ok(root),
			(false, false) => {
				if parts(&root.text).count() + parts(&self.text).count() > Pattern::MAX_SEGMENTS {
					return Err(InvalidPattern::TooManySegments.into());
				}
				Ok(Self {
					text: format!("{}/{}", root.text, self.text),
				})
			}
		}
	}

	/// This prefix as seen from the path `root`, normalized like a broadcast path: the rest
	/// beneath it, or the empty prefix when this one covers `root` already. `None` when they
	/// share no path.
	pub fn rebase(&self, root: &str) -> Option<Self> {
		// Segment by segment, so a root ending inside an escape matches no segment.
		let mut root = parts(root).filter(|part| !part.is_empty());
		let mut mine = parts(&self.text);
		let mut end = 0;
		loop {
			match (root.next(), mine.next()) {
				// This prefix ends at or above the root, so it covers all of it.
				(_, None) => return Some(Self::all()),
				// The root ends above this prefix: the rest of it is what lies beneath.
				(None, Some(_)) => {
					return Some(Self {
						text: self.text[end..].to_string(),
					});
				}
				(Some(theirs), Some(ours)) if theirs == ours => end += ours.len() + 1,
				(Some(_), Some(_)) => return None,
			}
		}
	}

	/// The paths this prefix covers, as a pattern: `prefix/**`, or the literal when it is
	/// already as deep as a path may be.
	pub fn pattern(&self) -> Pattern {
		Pattern::subtree(&self.text).expect("a prefix is a valid literal")
	}
}

/// Whether the canonical path `path` is `prefix` or beneath it.
///
/// A valid prefix never ends inside an escape, so the byte after it is a separator exactly
/// when it is a `/`.
fn beneath(path: &str, prefix: &str) -> bool {
	prefix.is_empty()
		|| (path.starts_with(prefix) && (path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/'))
}

/// `path` with its slashes trimmed and collapsed, as a broadcast path is normalized.
fn normalize(path: &str) -> std::borrow::Cow<'_, str> {
	if parts(path).all(|part| !part.is_empty()) {
		return path.into();
	}
	parts(path).filter(|part| !part.is_empty()).collect::<Vec<_>>().join("/").into()
}

/// Segment-by-segment order of two canonical paths.
fn order(a: &str, b: &str) -> Ordering {
	parts(a).cmp(parts(b))
}

impl Ord for Prefix {
	fn cmp(&self, other: &Self) -> Ordering {
		order(&self.text, &other.text)
	}
}

impl PartialOrd for Prefix {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl FromStr for Prefix {
	type Err = InvalidPrefix;

	/// Parse a prefix's text. A `*` anywhere is refused before anything else, so a pattern
	/// written where a prefix belongs always says so. Slashes are not normalized: a
	/// leading, trailing, or doubled `/` is an error, so a typo cannot silently widen a grant.
	fn from_str(text: &str) -> Result<Self, InvalidPrefix> {
		if text.contains('*') {
			return Err(InvalidPrefix::Wildcard(text.to_string()));
		}
		// One past the limit is enough to refuse, without splitting a peer's megabytes of
		// `a/a/...` first.
		let mut count = 0;
		for part in parts(text).take(Pattern::MAX_SEGMENTS + 1) {
			count += 1;
			if part.is_empty() {
				return Err(InvalidPattern::EmptySegment.into());
			}
			if !is_segment(part) {
				return Err(InvalidPattern::InvalidSegment(part.to_string()).into());
			}
		}
		if count > Pattern::MAX_SEGMENTS {
			return Err(InvalidPattern::TooManySegments.into());
		}
		Ok(Self { text: text.to_string() })
	}
}

impl TryFrom<&str> for Prefix {
	type Error = InvalidPrefix;

	fn try_from(text: &str) -> Result<Self, InvalidPrefix> {
		text.parse()
	}
}

impl TryFrom<String> for Prefix {
	type Error = InvalidPrefix;

	fn try_from(text: String) -> Result<Self, InvalidPrefix> {
		text.parse()
	}
}

impl fmt::Display for Prefix {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.text)
	}
}

impl fmt::Debug for Prefix {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Debug::fmt(&self.text, f)
	}
}

#[cfg(feature = "serde")]
impl serde::Serialize for Prefix {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(&self.text)
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Prefix {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

/// A grant: a set of prefixes, reduced so no member lies beneath another.
///
/// Order is canonical (see [`Prefix`]), so two sets covering the same paths compare equal.
/// The empty set grants nothing; [`all`](Self::all) grants everything.
///
/// Every operation costs near-linear time in the members it reads: collecting sorts once
/// and sweeps, and a query binary-searches each prefix above the path it asks about, of
/// which there are at most [`Pattern::MAX_SEGMENTS`] + 1. So a peer's grant costs
/// O(n log n) to read, whatever its shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Prefixes(Vec<Prefix>);

impl Prefixes {
	/// The empty set, which grants nothing.
	pub fn new() -> Self {
		Self::default()
	}

	/// The set of the empty prefix, which grants every path.
	pub fn all() -> Self {
		Self(vec![Prefix::all()])
	}

	/// Add a prefix, dropping the members beneath it. Returns `false` when a member already
	/// covers it, leaving the set unchanged.
	pub fn insert(&mut self, prefix: Prefix) -> bool {
		if self.matches(&prefix.text) {
			return false;
		}
		// Everything beneath the new member sorts right after where it goes.
		let at = self.0.partition_point(|member| member < &prefix);
		let end = at + self.0[at..].iter().take_while(|member| prefix.matches(&member.text)).count();
		self.0.splice(at..end, [prefix]);
		true
	}

	/// Whether some member covers the canonical `path`.
	pub fn matches(&self, path: &str) -> bool {
		self.covering(path).is_some()
	}

	/// The member covering the canonical `path`, if any.
	fn covering(&self, path: &str) -> Option<&Prefix> {
		if self.0.is_empty() {
			return None;
		}
		// Each prefix of the path at a segment boundary, the empty one first.
		let mut end = 0;
		let mut ancestors = std::iter::once(0).chain(parts(path).map(move |part| {
			end += usize::from(end > 0) + part.len();
			end
		}));
		ancestors.find_map(|end| {
			let ancestor = &path[..end];
			let index = self.0.binary_search_by(|member| order(&member.text, ancestor)).ok()?;
			Some(&self.0[index])
		})
	}

	/// Whether every member of `other` is covered here: `other` grants nothing this set
	/// does not. The empty set is covered by anything.
	pub fn covers(&self, other: &Self) -> bool {
		other.0.iter().all(|prefix| self.matches(&prefix.text))
	}

	/// Whether some member covers a path at or beneath the canonical `path`.
	pub fn overlaps(&self, path: &str) -> bool {
		if self.matches(path) {
			return true;
		}
		// Members beneath it sort right after it.
		let at = self.0.partition_point(|member| order(&member.text, path) == Ordering::Less);
		self.0.get(at).is_some_and(|member| beneath(&member.text, path))
	}

	/// The paths both sets grant. Each member of the result is a member of one side lying
	/// within the other, so it never grows past both sets together.
	pub fn intersect(&self, other: &Self) -> Self {
		let ours = self.0.iter().filter(|prefix| other.matches(&prefix.text));
		let theirs = other.0.iter().filter(|prefix| self.matches(&prefix.text));
		ours.chain(theirs).cloned().collect()
	}

	/// Every member as seen from the path `root`; see [`Prefix::rebase`].
	pub fn rebase(&self, root: &str) -> Self {
		self.0.iter().filter_map(|prefix| prefix.rebase(root)).collect()
	}

	/// Every member placed beneath the path `root`; see [`Prefix::rooted`].
	pub fn rooted(&self, root: &str) -> Result<Self, InvalidPrefix> {
		self.0.iter().map(|prefix| prefix.rooted(root)).collect()
	}

	/// The paths this set covers, as patterns: each member's [`Prefix::pattern`].
	pub fn patterns(&self) -> Patterns {
		self.0.iter().map(Prefix::pattern).collect()
	}

	/// The members, in canonical order.
	pub fn iter(&self) -> std::slice::Iter<'_, Prefix> {
		self.0.iter()
	}

	/// The number of members.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether there are no members, so nothing is granted.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}
}

impl From<Prefix> for Prefixes {
	fn from(prefix: Prefix) -> Self {
		Self(vec![prefix])
	}
}

impl FromIterator<Prefix> for Prefixes {
	/// Reduce in one sort and one sweep: a member's subtree sorts right after it, so each
	/// prefix is beneath a member kept already only if it is beneath the last one kept.
	fn from_iter<I: IntoIterator<Item = Prefix>>(iter: I) -> Self {
		let mut sorted: Vec<Prefix> = iter.into_iter().collect();
		sorted.sort_unstable();
		let mut kept: Vec<Prefix> = Vec::with_capacity(sorted.len());
		for prefix in sorted {
			if kept.last().is_none_or(|last| !last.matches(&prefix.text)) {
				kept.push(prefix);
			}
		}
		kept.shrink_to_fit();
		Self(kept)
	}
}

impl Extend<Prefix> for Prefixes {
	/// Reduce the members and the new prefixes together, in one pass.
	fn extend<I: IntoIterator<Item = Prefix>>(&mut self, iter: I) {
		let members = std::mem::take(&mut self.0);
		*self = members.into_iter().chain(iter).collect();
	}
}

impl IntoIterator for Prefixes {
	type Item = Prefix;
	type IntoIter = std::vec::IntoIter<Prefix>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Prefixes {
	type Item = &'a Prefix;
	type IntoIter = std::slice::Iter<'a, Prefix>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.iter()
	}
}

#[cfg(feature = "serde")]
impl serde::Serialize for Prefixes {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_seq(self.iter())
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Prefixes {
	/// Reads a list and reduces it, so a persisted grant is canonical after a round trip.
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Ok(Vec::<Prefix>::deserialize(deserializer)?.into_iter().collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn prefix(text: &str) -> Prefix {
		text.parse().unwrap()
	}

	fn prefixes(texts: &[&str]) -> Prefixes {
		texts.iter().map(|text| prefix(text)).collect()
	}

	#[test]
	fn refuses_every_wildcard_loudly() {
		for text in ["**", "*", "a/**", "a/*", "cam-*.hang", "a*", "*/chat", "**/a", r"a\*"] {
			let err = text.parse::<Prefix>().unwrap_err();
			assert_eq!(err, InvalidPrefix::Wildcard(text.to_string()), "{text}");
		}
		assert!(InvalidPrefix::Wildcard("**".into()).to_string().contains("empty prefix"));
		assert!(InvalidPrefix::Wildcard("anon/**".into()).to_string().contains("`anon` covers"));
		assert!(InvalidPrefix::Wildcard("a/*/b".into()).to_string().contains("holds a wildcard"));
	}

	#[test]
	fn refuses_what_is_not_a_path() {
		for text in ["/a", "a/", "a//b", r"a\", r"a\b"] {
			assert!(matches!(text.parse::<Prefix>(), Err(InvalidPrefix::Path(_))), "{text}");
		}
		let deep = ["a"; Pattern::MAX_SEGMENTS + 1].join("/");
		assert!(deep.parse::<Prefix>().is_err());
		assert!(["a"; Pattern::MAX_SEGMENTS].join("/").parse::<Prefix>().is_ok());
		assert!(prefix("").is_empty());
		assert_eq!(prefix(r"a\/b/c\\").as_str(), r"a\/b/c\\");
	}

	#[test]
	fn covers_its_subtree_by_segment() {
		let room = prefix("room");
		assert!(room.matches("room"));
		assert!(room.matches("room/alice"));
		assert!(!room.matches("roomy"));
		assert!(!room.matches("room-b/x"));
		assert!(!room.matches(""));
		assert!(Prefix::all().matches(""));
		assert!(Prefix::all().matches("anything/at/all"));
		// An escaped `/` never splits a segment.
		assert!(!prefix("a").matches(r"a\/b"));
		assert!(prefix(r"a\/b").matches(r"a\/b/c"));
		assert!(prefix(r"a\\").matches(r"a\\/b"));
		assert!(!prefix(r"a\/").matches(r"a\/b"));
		assert!(prefix("room/a").overlaps("room"));
		assert!(room.overlaps("room/a"));
		assert!(!prefix("a").overlaps("b"));
		// A path no prefix can spell still has an answer.
		assert!(room.overlaps("room/cam-*"));
	}

	#[test]
	fn sorts_a_subtree_right_after_its_prefix() {
		let mut sorted = [prefix("a/b"), prefix("a-b"), prefix("a"), prefix("a/b/c"), prefix("")];
		sorted.sort();
		let texts: Vec<_> = sorted.iter().map(Prefix::as_str).collect();
		assert_eq!(texts, ["", "a", "a/b", "a/b/c", "a-b"]);
	}

	#[test]
	fn collecting_reduces_and_sorts() {
		assert_eq!(prefixes(&["a/b", "a-b", "a", "c/d"]), prefixes(&["c/d", "a-b", "a"]));
		assert_eq!(prefixes(&["a/b", "", "c"]), Prefixes::all());
		assert_eq!(prefixes(&["b", "a", "a"]).len(), 2);
		let set = prefixes(&["b", "a/x", "a-b"]);
		let texts: Vec<_> = set.iter().map(Prefix::as_str).collect();
		assert_eq!(texts, ["a/x", "a-b", "b"]);
	}

	#[test]
	fn insert_drops_what_it_covers() {
		let mut set = prefixes(&["a/b", "a/c", "a-b", "b"]);
		assert!(!set.insert(prefix("a/b/x")));
		assert!(set.insert(prefix("a")));
		assert_eq!(set, prefixes(&["a", "a-b", "b"]));
		assert!(set.insert(prefix("")));
		assert_eq!(set, Prefixes::all());
	}

	#[test]
	fn queries_any_member() {
		let set = prefixes(&["a/b", "c"]);
		assert!(set.matches("a/b/x"));
		assert!(set.matches("c"));
		assert!(!set.matches("a"));
		assert!(!set.matches("a/bc"));
		assert!(set.matches("c/d"));
		assert!(set.overlaps("a"));
		assert!(set.overlaps("a/b/c"));
		assert!(set.overlaps(""));
		assert!(!set.overlaps("a/c"));
		assert!(!set.overlaps("b"));
		assert!(Prefixes::all().overlaps("z"));
		assert!(!Prefixes::new().matches(""));
		assert!(!Prefixes::new().overlaps(""));
	}

	#[test]
	fn covers_and_intersects() {
		let wide = prefixes(&["a", "b/c"]);
		assert!(wide.covers(&prefixes(&["a/x", "b/c/d"])));
		assert!(!wide.covers(&prefixes(&["a/x", "b"])));
		assert!(wide.covers(&Prefixes::new()));
		assert!(!Prefixes::new().covers(&wide));
		assert_eq!(wide.intersect(&prefixes(&["a/x", "b", "z"])), prefixes(&["a/x", "b/c"]));
		assert_eq!(wide.intersect(&Prefixes::all()), wide);
		assert!(wide.intersect(&prefixes(&["z"])).is_empty());
	}

	#[test]
	fn rebases_and_roots() {
		let set = prefixes(&["a/b", "c"]);
		assert_eq!(set.rebase("a"), prefixes(&["b"]));
		assert_eq!(set.rebase("c/d"), Prefixes::all());
		assert_eq!(set.rebase(""), set);
		assert!(set.rebase("x").is_empty());
		assert_eq!(set.rooted("r").unwrap(), prefixes(&["r/a/b", "r/c"]));
		assert_eq!(Prefixes::all().rooted("r").unwrap(), prefixes(&["r"]));
		assert!(set.rooted("*").is_err());
		let deep = ["a"; Pattern::MAX_SEGMENTS].join("/");
		assert!(set.rooted(&deep).is_err());
		assert_eq!(set.rooted("r").unwrap().rebase("r"), set);
		// A root is normalized like a broadcast path, as a pattern's is.
		assert_eq!(set.rebase("/a//"), prefixes(&["b"]));
		assert_eq!(set.rooted("/r//s/").unwrap(), prefixes(&["r/s/a/b", "r/s/c"]));
		// A root ending inside an escape is no segment of a prefix.
		let escaped = prefixes(&[r"a\/b"]);
		assert!(escaped.rebase(r"a\").is_empty());
		assert_eq!(escaped.rebase(r"a\/b"), Prefixes::all());
	}

	#[test]
	fn patterns_cover_the_same_paths() {
		let set = prefixes(&["a", ""]);
		assert_eq!(set.patterns(), Patterns::from(Pattern::all()));
		assert_eq!(prefixes(&["a/b"]).patterns(), Patterns::from(Pattern::subtree("a/b").unwrap()));
		let deep = ["a"; Pattern::MAX_SEGMENTS].join("/");
		assert_eq!(prefix(&deep).pattern(), Pattern::literal(&deep).unwrap());
	}

	#[cfg(feature = "serde")]
	#[test]
	fn serde_round_trips_reduced_and_refuses_wildcards() {
		let set: Prefixes = serde_json::from_str(r#"["a/b", "a", "c"]"#).unwrap();
		assert_eq!(serde_json::to_string(&set).unwrap(), r#"["a","c"]"#);
		let err = serde_json::from_str::<Prefixes>(r#"["a/**"]"#).unwrap_err().to_string();
		assert!(err.contains("`a` covers"), "{err}");
	}

	/// Every operation agrees with a brute-force reading over a small universe of paths.
	#[test]
	fn agrees_with_brute_force() {
		let universe = ["", "a", "a/b", "a/b/c", "a/c", "a-b", "b", "b/a"];
		let paths: Vec<&str> = universe.to_vec();
		// Every subset of a few members.
		let members = ["", "a", "a/b", "a-b", "b/a"];
		let sets: Vec<Prefixes> = (0..1u32 << members.len())
			.map(|mask| {
				(0..members.len())
					.filter(|bit| mask & (1 << bit) != 0)
					.map(|bit| prefix(members[bit]))
					.collect()
			})
			.collect();
		let covered = |set: &Prefixes| -> Vec<&str> { paths.iter().copied().filter(|path| set.matches(path)).collect() };
		let brute = |set: &Prefixes, path: &str| set.iter().any(|member| member.matches(path));
		for a in &sets {
			for path in &paths {
				assert_eq!(a.matches(path), brute(a, path), "{a:?} {path}");
				let p = prefix(path);
				let beneath: Vec<_> = paths.iter().filter(|other| p.matches(other)).collect();
				// Covering a path covers its whole subtree.
				assert_eq!(a.matches(path), beneath.iter().all(|other| brute(a, other)), "{a:?} covers {path}");
				assert_eq!(a.overlaps(path), beneath.iter().any(|other| brute(a, other)), "{a:?} overlaps {path}");
			}
			for b in &sets {
				let both = a.intersect(b);
				let want: Vec<&str> = paths.iter().copied().filter(|path| brute(a, path) && brute(b, path)).collect();
				assert_eq!(covered(&both), want, "{a:?} & {b:?}");
				let a_paths = covered(a);
				assert_eq!(a.covers(b), covered(b).iter().all(|path| a_paths.contains(path)), "{a:?} covers {b:?}");
			}
		}
	}
}
