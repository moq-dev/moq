use std::borrow::Cow;
use std::fmt::{self, Display};
use std::sync::Arc;

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

/// An owned version of [`Path`] with a `'static` lifetime.
pub type PathOwned = Path<'static>;

/// A trait for types that can be converted to a `Path`.
///
/// When providing a String/str, any leading/trailing slashes are trimmed and multiple consecutive slashes are collapsed.
/// When already a Path, normalization is skipped and the underlying buffer is reused without copying.
pub trait AsPath {
	fn as_path(&self) -> Path<'_>;
}

impl<'a> AsPath for &'a str {
	fn as_path(&self) -> Path<'a> {
		Path::new(self)
	}
}

impl<'a> AsPath for &'a Path<'a> {
	fn as_path(&self) -> Path<'a> {
		// We don't normalize again nor do we copy the bytes.
		self.borrow()
	}
}

impl AsPath for Path<'_> {
	fn as_path(&self) -> Path<'_> {
		self.borrow()
	}
}

impl AsPath for String {
	fn as_path(&self) -> Path<'_> {
		Path::new(self)
	}
}

impl<'a> AsPath for &'a String {
	fn as_path(&self) -> Path<'a> {
		Path::new(self)
	}
}

/// A borrowed slice of the path, or a suffix of a shared reference-counted buffer.
///
/// The `Shared` variant is what makes owned paths cheap: cloning bumps a refcount and
/// suffix operations (strip_prefix, next_part) only advance `start`, so one allocation
/// serves every copy of a path as it fans out to consumers.
#[derive(Clone)]
enum Repr<'a> {
	Borrowed(&'a str),
	Shared { buf: Arc<str>, start: usize },
}

/// A broadcast path that provides safe prefix matching operations.
///
/// This type wraps a string but provides path-aware operations that respect
/// delimiter boundaries, preventing issues like "foo" matching "foobar".
///
/// Paths are automatically trimmed of leading and trailing slashes on creation,
/// making all slashes implicit at boundaries.
/// All paths are RELATIVE; you cannot join with a leading slash to make an absolute path.
///
/// Owned paths ([`PathOwned`]) share one reference-counted allocation: cloning, converting
/// a shared path with [`Path::to_owned`], and suffix operations like [`Path::strip_prefix`]
/// do not copy the underlying bytes.
///
/// # Examples
/// ```
/// use moq_net::{Path};
///
/// // Creation automatically trims slashes
/// let path1 = Path::new("/foo/bar/");
/// let path2 = Path::new("foo/bar");
/// assert_eq!(path1, path2);
///
/// // Methods accept both &str and Path
/// let base = Path::new("api/v1");
/// assert!(base.has_prefix("api"));
/// assert!(base.has_prefix(&Path::new("api/v1")));
///
/// let joined = base.join("users");
/// assert_eq!(joined.as_str(), "api/v1/users");
/// ```
#[derive(Clone)]
pub struct Path<'a>(Repr<'a>);

impl<'a> Path<'a> {
	/// Maximum number of slash-separated parts in a path.
	///
	/// Matches the IETF moq-transport limit of 32 fields in a namespace tuple.
	/// moq-lite enforces the same bound: encoding or decoding a deeper path fails,
	/// and publishing one to an origin is rejected.
	pub const MAX_PARTS: usize = 32;

	/// Create a new Path from a string slice.
	///
	/// Leading and trailing slashes are automatically trimmed.
	/// Multiple consecutive internal slashes are collapsed to single slashes.
	pub fn new(s: &'a str) -> Self {
		let trimmed = s.trim_start_matches('/').trim_end_matches('/');

		// Check if we need to normalize (has multiple consecutive slashes)
		if trimmed.contains("//") {
			// Only allocate if we actually need to normalize
			let normalized = trimmed
				.split('/')
				.filter(|s| !s.is_empty())
				.collect::<Vec<_>>()
				.join("/");
			Self(Repr::Shared {
				buf: normalized.into(),
				start: 0,
			})
		} else {
			// No normalization needed - use borrowed string
			Self(Repr::Borrowed(trimmed))
		}
	}

	// A copy of this path skipping the first `n` bytes, reusing the shared buffer when possible.
	fn slice_from(&'a self, n: usize) -> Path<'a> {
		match &self.0 {
			Repr::Borrowed(s) => Path(Repr::Borrowed(&s[n..])),
			Repr::Shared { buf, start } => Path(Repr::Shared {
				buf: buf.clone(),
				start: start + n,
			}),
		}
	}

	/// Check if this path has the given prefix, respecting path boundaries.
	///
	/// Unlike String::starts_with, this ensures that "foo" does not match "foobar".
	/// The prefix must either:
	/// - Be exactly equal to this path
	/// - Be followed by a '/' delimiter in the original path
	/// - Be empty (matches everything)
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// let path = Path::new("foo/bar");
	/// assert!(path.has_prefix("foo"));
	/// assert!(path.has_prefix(&Path::new("foo")));
	/// assert!(path.has_prefix("foo/"));
	/// assert!(!path.has_prefix("fo"));
	///
	/// let path = Path::new("foobar");
	/// assert!(!path.has_prefix("foo"));
	/// ```
	pub fn has_prefix(&self, prefix: impl AsPath) -> bool {
		let prefix = prefix.as_path();

		if prefix.is_empty() {
			return true;
		}

		let s = self.as_str();
		if !s.starts_with(prefix.as_str()) {
			return false;
		}

		// Check if the prefix is the exact match
		if s.len() == prefix.len() {
			return true;
		}

		// Otherwise, ensure the character after the prefix is a delimiter
		s.as_bytes().get(prefix.len()) == Some(&b'/')
	}

	pub fn strip_prefix(&'a self, prefix: impl AsPath) -> Option<Path<'a>> {
		let prefix = prefix.as_path();

		if prefix.is_empty() {
			return Some(self.borrow());
		}

		let s = self.as_str();
		if !s.starts_with(prefix.as_str()) {
			return None;
		}

		// Check if the prefix is the exact match
		if s.len() == prefix.len() {
			return Some(Path::empty());
		}

		// Otherwise, ensure the character after the prefix is a delimiter
		if s.as_bytes().get(prefix.len()) != Some(&b'/') {
			return None;
		}

		Some(self.slice_from(prefix.len() + 1))
	}

	/// Iterate over the slash-separated parts of the path.
	///
	/// The empty path has no parts.
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// let path = Path::new("foo/bar/baz");
	/// assert_eq!(path.parts().collect::<Vec<_>>(), ["foo", "bar", "baz"]);
	/// assert_eq!(Path::empty().parts().count(), 0);
	/// ```
	pub fn parts(&self) -> impl Iterator<Item = &str> {
		// Paths are normalized on creation so there are no empty parts to filter,
		// except that splitting the empty path yields one empty item.
		self.as_str().split('/').filter(|part| !part.is_empty())
	}

	/// Strip the directory component of the path, if any, and return the rest of the path.
	pub fn next_part(&'a self) -> Option<(&'a str, Path<'a>)> {
		let s = self.as_str();
		if s.is_empty() {
			return None;
		}

		if let Some(i) = s.find('/') {
			Some((&s[..i], self.slice_from(i + 1)))
		} else {
			Some((s, Path::empty()))
		}
	}

	pub fn as_str(&self) -> &str {
		match &self.0 {
			Repr::Borrowed(s) => s,
			Repr::Shared { buf, start } => &buf[*start..],
		}
	}

	pub fn empty() -> Path<'static> {
		Path(Repr::Borrowed(""))
	}

	pub fn is_empty(&self) -> bool {
		self.as_str().is_empty()
	}

	pub fn len(&self) -> usize {
		self.as_str().len()
	}

	pub fn to_owned(&self) -> PathOwned {
		match &self.0 {
			Repr::Borrowed("") => Path::empty(),
			Repr::Borrowed(s) => Path(Repr::Shared {
				buf: Arc::from(*s),
				start: 0,
			}),
			Repr::Shared { buf, start } => Path(Repr::Shared {
				buf: buf.clone(),
				start: *start,
			}),
		}
	}

	pub fn into_owned(self) -> PathOwned {
		match self.0 {
			Repr::Borrowed("") => Path::empty(),
			Repr::Borrowed(s) => Path(Repr::Shared {
				buf: Arc::from(s),
				start: 0,
			}),
			Repr::Shared { buf, start } => Path(Repr::Shared { buf, start }),
		}
	}

	/// A copy of this path bound to `self`'s lifetime, without copying the underlying bytes.
	pub fn borrow(&'a self) -> Path<'a> {
		self.slice_from(0)
	}

	/// Join this path with another path component.
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// let base = Path::new("foo");
	/// let joined = base.join("bar");
	/// assert_eq!(joined.as_str(), "foo/bar");
	///
	/// let joined = base.join(&Path::new("bar"));
	/// assert_eq!(joined.as_str(), "foo/bar");
	/// ```
	pub fn join(&self, other: impl AsPath) -> PathOwned {
		let other = other.as_path();

		if self.is_empty() {
			other.to_owned()
		} else if other.is_empty() {
			self.to_owned()
		} else {
			// Since paths are trimmed, we always need to add a slash
			Path(Repr::Shared {
				buf: format!("{}/{}", self.as_str(), other.as_str()).into(),
				start: 0,
			})
		}
	}

	/// Resolve a [`PathRelative`] against this path.
	///
	/// `..` segments in `rel` pop the last segment of the base; other segments are appended.
	/// Excess `..` is a no-op once the base is empty (subsequent named segments still append).
	/// An empty `rel` returns this path as an owned copy.
	///
	/// [`PathRelative::new`] strips `.` and empty segments, so they are not handled here.
	///
	/// # Examples
	/// ```
	/// use moq_net::{Path, PathRelative};
	///
	/// let base = Path::new("a/b/c");
	/// assert_eq!(base.resolve(&PathRelative::new("../d")).as_str(), "a/b/d");
	/// assert_eq!(base.resolve(&PathRelative::new("d")).as_str(), "a/b/c/d");
	/// assert_eq!(base.resolve(&PathRelative::new("../../../../x")).as_str(), "x");
	/// ```
	pub fn resolve(&self, rel: &PathRelative<'_>) -> PathOwned {
		if rel.is_empty() {
			return self.to_owned();
		}

		let mut segments: Vec<&str> = self.parts().collect();

		for seg in rel.as_str().split('/') {
			if seg == ".." {
				segments.pop();
			} else {
				segments.push(seg);
			}
		}

		let path = segments.join("/");
		if path.is_empty() {
			Path::empty()
		} else {
			Path(Repr::Shared {
				buf: path.into(),
				start: 0,
			})
		}
	}
}

// Comparisons, ordering, and hashing all go through `as_str()` so a borrowed and a
// shared path with the same content behave identically (e.g. as map keys).
impl<'b> PartialEq<Path<'b>> for Path<'_> {
	fn eq(&self, other: &Path<'b>) -> bool {
		self.as_str() == other.as_str()
	}
}

impl Eq for Path<'_> {}

impl PartialOrd for Path<'_> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Path<'_> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.as_str().cmp(other.as_str())
	}
}

impl std::hash::Hash for Path<'_> {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.as_str().hash(state)
	}
}

impl fmt::Debug for Path<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Path").field(&self.as_str()).finish()
	}
}

impl serde::Serialize for Path<'_> {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'a> From<&'a str> for Path<'a> {
	fn from(s: &'a str) -> Self {
		Self::new(s)
	}
}

impl<'a> From<&'a String> for Path<'a> {
	fn from(s: &'a String) -> Self {
		// TODO avoid making a copy here
		Self::new(s)
	}
}

impl Default for Path<'_> {
	fn default() -> Self {
		Path::empty()
	}
}

impl From<String> for Path<'_> {
	fn from(s: String) -> Self {
		Path::new(&s).into_owned()
	}
}

impl AsRef<str> for Path<'_> {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

impl Display for Path<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.as_str())
	}
}

impl<V: Copy> Decode<V> for Path<'_>
where
	String: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let path: Path = String::decode(r, version)?.into();
		if path.parts().count() > Path::MAX_PARTS {
			return Err(DecodeError::BoundsExceeded);
		}
		Ok(path)
	}
}

impl<V: Copy> Encode<V> for Path<'_>
where
	for<'a> &'a str: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		if self.parts().count() > Path::MAX_PARTS {
			return Err(EncodeError::BoundsExceeded);
		}
		self.as_str().encode(w, version)?;
		Ok(())
	}
}

/// An owned version of [`PathRelative`] with a `'static` lifetime.
pub type PathRelativeOwned = PathRelative<'static>;

/// A relative broadcast path, used to reference one broadcast from another broadcast's content.
///
/// Unlike [`Path`] (which is a complete reference within the broadcast namespace),
/// `PathRelative` may contain `..` segments to walk up the namespace and is meaningful only
/// when resolved against a base [`Path`] via [`Path::resolve`]. The hang catalog uses it to
/// point a rendition at a track published in a sibling broadcast (e.g. `../source`).
///
/// `PathRelative` has no `Encode`/`Decode` impl, so it never appears in announce/subscribe
/// frames. It does serialize via serde for off-wire use (e.g. as a field inside a catalog
/// JSON payload, which itself travels as a track).
///
/// Normalization on creation: leading/trailing slashes are trimmed, consecutive internal
/// slashes collapse to one, and `.` segments are stripped (treated as no-ops, matching
/// POSIX). `..` is preserved and is interpreted at resolve time.
///
/// # Examples
/// ```
/// use moq_net::{Path, PathRelative};
///
/// let rel = PathRelative::new("../source");
/// assert_eq!(Path::new("a/b").resolve(&rel).as_str(), "a/source");
///
/// // `.` segments are stripped on creation.
/// assert_eq!(PathRelative::new("./a/./b").as_str(), "a/b");
/// ```
#[derive(Debug, PartialEq, Eq, Hash, Clone, serde::Serialize)]
pub struct PathRelative<'a>(Cow<'a, str>);

impl<'a> PathRelative<'a> {
	/// Create a new `PathRelative` from a string slice.
	///
	/// Leading and trailing slashes are trimmed, consecutive internal slashes collapse to one,
	/// and `.` segments are stripped. See the type-level doc for the full normalization rules.
	pub fn new(s: &'a str) -> Self {
		let trimmed = s.trim_start_matches('/').trim_end_matches('/');

		if needs_normalize_relative(trimmed) {
			Self(Cow::Owned(normalize_relative_segments(trimmed)))
		} else {
			Self(Cow::Borrowed(trimmed))
		}
	}

	/// The normalized path as a string slice.
	pub fn as_str(&self) -> &str {
		&self.0
	}

	/// The empty relative path, which resolves to the base path itself.
	pub fn empty() -> PathRelative<'static> {
		PathRelative(Cow::Borrowed(""))
	}

	/// True if the path is empty (resolves to the base path itself).
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// The length of the normalized path in bytes.
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Copy into an owned version with a `'static` lifetime.
	pub fn to_owned(&self) -> PathRelativeOwned {
		PathRelative(Cow::Owned(self.0.to_string()))
	}

	/// Convert into an owned version with a `'static` lifetime.
	pub fn into_owned(self) -> PathRelativeOwned {
		PathRelative(Cow::Owned(self.0.into_owned()))
	}

	/// Reborrow without copying.
	pub fn borrow(&'a self) -> PathRelative<'a> {
		PathRelative(Cow::Borrowed(&self.0))
	}
}

impl<'a> From<&'a str> for PathRelative<'a> {
	fn from(s: &'a str) -> Self {
		Self::new(s)
	}
}

impl<'a> From<&'a String> for PathRelative<'a> {
	fn from(s: &'a String) -> Self {
		Self::new(s)
	}
}

impl From<String> for PathRelative<'_> {
	fn from(s: String) -> Self {
		let trimmed = s.trim_start_matches('/').trim_end_matches('/');

		if needs_normalize_relative(trimmed) {
			Self(Cow::Owned(normalize_relative_segments(trimmed)))
		} else if trimmed == s {
			Self(Cow::Owned(s))
		} else {
			Self(Cow::Owned(trimmed.to_string()))
		}
	}
}

fn needs_normalize_relative(trimmed: &str) -> bool {
	trimmed.split('/').any(|seg| seg.is_empty() || seg == ".")
}

fn normalize_relative_segments(trimmed: &str) -> String {
	trimmed
		.split('/')
		.filter(|seg| !seg.is_empty() && *seg != ".")
		.collect::<Vec<_>>()
		.join("/")
}

impl Default for PathRelative<'_> {
	fn default() -> Self {
		Self(Cow::Borrowed(""))
	}
}

impl AsRef<str> for PathRelative<'_> {
	fn as_ref(&self) -> &str {
		&self.0
	}
}

impl Display for PathRelative<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.0)
	}
}

// Owned-only deserialization. We use `String::deserialize` so that owned deserializers
// (e.g. `serde_json::from_slice`) work. The borrowed form `<&str>::deserialize` requires
// `'de: 'a`, which is unsatisfiable when `'a = 'static`.
impl<'de> serde::Deserialize<'de> for PathRelative<'static> {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let s = String::deserialize(deserializer)?;
		Ok(PathRelative::from(s))
	}
}

/// A deduplicated list of path prefixes.
///
/// Automatically removes exact duplicates and overlapping prefixes on construction.
/// For example, `["demo", "demo/foo", "anon"]` becomes `["demo", "anon"]` since
/// `"demo"` already covers `"demo/foo"`.
#[derive(Debug, Clone, Default, Eq)]
pub struct PathPrefixes {
	paths: Vec<PathOwned>,
}

impl PathPrefixes {
	/// Create a new PathPrefixes, deduplicating and removing overlapping prefixes.
	///
	/// Shorter prefixes subsume longer ones: `"demo"` covers `"demo/foo"`.
	///
	/// Accepts anything iterable over path-like items:
	/// ```
	/// use moq_net::PathPrefixes;
	///
	/// let list = PathPrefixes::new(["demo", "demo/foo", "anon"]);
	/// assert_eq!(list.len(), 2); // "demo/foo" subsumed by "demo"
	/// ```
	pub fn new(paths: impl IntoIterator<Item = impl AsPath>) -> Self {
		let mut paths: Vec<PathOwned> = paths.into_iter().map(|p| p.as_path().to_owned()).collect();

		if paths.len() <= 1 {
			return Self { paths };
		}

		// Sort by length so shorter (more permissive) prefixes come first.
		// Tie-break lexicographically for canonical ordering.
		paths.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.as_str().cmp(b.as_str())));
		paths.dedup();

		let mut result: Vec<PathOwned> = Vec::new();
		'outer: for path in paths {
			for existing in &result {
				if path.has_prefix(existing) {
					continue 'outer;
				}
			}
			result.push(path);
		}

		Self { paths: result }
	}

	pub fn is_empty(&self) -> bool {
		self.paths.is_empty()
	}

	pub fn len(&self) -> usize {
		self.paths.len()
	}

	pub fn iter(&self) -> std::slice::Iter<'_, PathOwned> {
		self.paths.iter()
	}
}

impl std::ops::Deref for PathPrefixes {
	type Target = [PathOwned];

	fn deref(&self) -> &[PathOwned] {
		&self.paths
	}
}

impl FromIterator<PathOwned> for PathPrefixes {
	fn from_iter<I: IntoIterator<Item = PathOwned>>(iter: I) -> Self {
		Self::new(iter)
	}
}

impl From<Vec<PathOwned>> for PathPrefixes {
	fn from(paths: Vec<PathOwned>) -> Self {
		Self::new(paths)
	}
}

impl<'a> PartialEq<Vec<Path<'a>>> for PathPrefixes {
	fn eq(&self, other: &Vec<Path<'a>>) -> bool {
		self.paths == *other
	}
}

impl<'a> PartialEq<PathPrefixes> for Vec<Path<'a>> {
	fn eq(&self, other: &PathPrefixes) -> bool {
		*self == other.paths
	}
}

impl PartialEq for PathPrefixes {
	fn eq(&self, other: &Self) -> bool {
		self.paths == other.paths
	}
}

impl IntoIterator for PathPrefixes {
	type Item = PathOwned;
	type IntoIter = std::vec::IntoIter<PathOwned>;

	fn into_iter(self) -> Self::IntoIter {
		self.paths.into_iter()
	}
}

impl<'a> IntoIterator for &'a PathPrefixes {
	type Item = &'a PathOwned;
	type IntoIter = std::slice::Iter<'a, PathOwned>;

	fn into_iter(self) -> Self::IntoIter {
		self.paths.iter()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_has_prefix() {
		let path = Path::new("foo/bar/baz");

		// Valid prefixes - test with both &str and &Path
		assert!(path.has_prefix(""));
		assert!(path.has_prefix("foo"));
		assert!(path.has_prefix(Path::new("foo")));
		assert!(path.has_prefix("foo/"));
		assert!(path.has_prefix("foo/bar"));
		assert!(path.has_prefix(Path::new("foo/bar/")));
		assert!(path.has_prefix("foo/bar/baz"));

		// Invalid prefixes - should not match partial components
		assert!(!path.has_prefix("f"));
		assert!(!path.has_prefix(Path::new("fo")));
		assert!(!path.has_prefix("foo/b"));
		assert!(!path.has_prefix("foo/ba"));
		assert!(!path.has_prefix(Path::new("foo/bar/ba")));

		// Edge case: "foobar" should not match "foo"
		let path = Path::new("foobar");
		assert!(!path.has_prefix("foo"));
		assert!(path.has_prefix(Path::new("foobar")));
	}

	#[test]
	fn test_strip_prefix() {
		let path = Path::new("foo/bar/baz");

		// Test with both &str and &Path
		assert_eq!(path.strip_prefix("").unwrap().as_str(), "foo/bar/baz");
		assert_eq!(path.strip_prefix("foo").unwrap().as_str(), "bar/baz");
		assert_eq!(path.strip_prefix(Path::new("foo/")).unwrap().as_str(), "bar/baz");
		assert_eq!(path.strip_prefix("foo/bar").unwrap().as_str(), "baz");
		assert_eq!(path.strip_prefix(Path::new("foo/bar/")).unwrap().as_str(), "baz");
		assert_eq!(path.strip_prefix("foo/bar/baz").unwrap().as_str(), "");

		// Should fail for invalid prefixes
		assert!(path.strip_prefix("fo").is_none());
		assert!(path.strip_prefix(Path::new("bar")).is_none());
	}

	#[test]
	fn test_join() {
		// Test with both &str and &Path
		assert_eq!(Path::new("foo").join("bar").as_str(), "foo/bar");
		assert_eq!(Path::new("foo/").join(Path::new("bar")).as_str(), "foo/bar");
		assert_eq!(Path::new("").join("bar").as_str(), "bar");
		assert_eq!(Path::new("foo/bar").join(Path::new("baz")).as_str(), "foo/bar/baz");
	}

	#[test]
	fn test_empty() {
		let empty = Path::new("");
		assert!(empty.is_empty());
		assert_eq!(empty.len(), 0);

		let non_empty = Path::new("foo");
		assert!(!non_empty.is_empty());
		assert_eq!(non_empty.len(), 3);
	}

	#[test]
	fn test_from_conversions() {
		let path1 = Path::from("foo/bar");
		let path2 = Path::from("foo/bar");
		let s = String::from("foo/bar");
		let path3 = Path::from(&s);

		assert_eq!(path1.as_str(), "foo/bar");
		assert_eq!(path2.as_str(), "foo/bar");
		assert_eq!(path3.as_str(), "foo/bar");
	}

	#[test]
	fn test_path_prefix_join() {
		let prefix = Path::new("foo");
		let suffix = Path::new("bar/baz");
		let path = prefix.join(&suffix);
		assert_eq!(path.as_str(), "foo/bar/baz");

		let prefix = Path::new("foo/");
		let suffix = Path::new("bar/baz");
		let path = prefix.join(&suffix);
		assert_eq!(path.as_str(), "foo/bar/baz");

		let prefix = Path::new("foo");
		let suffix = Path::new("/bar/baz");
		let path = prefix.join(&suffix);
		assert_eq!(path.as_str(), "foo/bar/baz");

		let prefix = Path::new("");
		let suffix = Path::new("bar/baz");
		let path = prefix.join(&suffix);
		assert_eq!(path.as_str(), "bar/baz");
	}

	#[test]
	fn test_path_prefix_conversions() {
		let prefix1 = Path::from("foo/bar");
		let prefix2 = Path::from(String::from("foo/bar"));
		let s = String::from("foo/bar");
		let prefix3 = Path::from(&s);

		assert_eq!(prefix1.as_str(), "foo/bar");
		assert_eq!(prefix2.as_str(), "foo/bar");
		assert_eq!(prefix3.as_str(), "foo/bar");
	}

	#[test]
	fn test_path_suffix_conversions() {
		let suffix1 = Path::from("foo/bar");
		let suffix2 = Path::from(String::from("foo/bar"));
		let s = String::from("foo/bar");
		let suffix3 = Path::from(&s);

		assert_eq!(suffix1.as_str(), "foo/bar");
		assert_eq!(suffix2.as_str(), "foo/bar");
		assert_eq!(suffix3.as_str(), "foo/bar");
	}

	#[test]
	fn test_path_types_basic_operations() {
		let prefix = Path::new("foo/bar");
		assert_eq!(prefix.as_str(), "foo/bar");
		assert!(!prefix.is_empty());
		assert_eq!(prefix.len(), 7);

		let suffix = Path::new("baz/qux");
		assert_eq!(suffix.as_str(), "baz/qux");
		assert!(!suffix.is_empty());
		assert_eq!(suffix.len(), 7);

		let empty_prefix = Path::new("");
		assert!(empty_prefix.is_empty());
		assert_eq!(empty_prefix.len(), 0);

		let empty_suffix = Path::new("");
		assert!(empty_suffix.is_empty());
		assert_eq!(empty_suffix.len(), 0);
	}

	#[test]
	fn test_prefix_has_prefix() {
		// Test empty prefix (should match everything)
		let prefix = Path::new("foo/bar");
		assert!(prefix.has_prefix(""));

		// Test exact matches
		let prefix = Path::new("foo/bar");
		assert!(prefix.has_prefix("foo/bar"));

		// Test valid prefixes
		assert!(prefix.has_prefix("foo"));
		assert!(prefix.has_prefix("foo/"));

		// Test invalid prefixes - partial matches should fail
		assert!(!prefix.has_prefix("f"));
		assert!(!prefix.has_prefix("fo"));
		assert!(!prefix.has_prefix("foo/b"));
		assert!(!prefix.has_prefix("foo/ba"));

		// Test edge cases
		let prefix = Path::new("foobar");
		assert!(!prefix.has_prefix("foo"));
		assert!(prefix.has_prefix("foobar"));

		// Test trailing slash handling
		let prefix = Path::new("foo/bar/");
		assert!(prefix.has_prefix("foo"));
		assert!(prefix.has_prefix("foo/"));
		assert!(prefix.has_prefix("foo/bar"));
		assert!(prefix.has_prefix("foo/bar/"));

		// Test single component
		let prefix = Path::new("foo");
		assert!(prefix.has_prefix(""));
		assert!(prefix.has_prefix("foo"));
		assert!(prefix.has_prefix("foo/")); // "foo/" becomes "foo" after trimming
		assert!(!prefix.has_prefix("f"));

		// Test empty prefix
		let prefix = Path::new("");
		assert!(prefix.has_prefix(""));
		assert!(!prefix.has_prefix("foo"));
	}

	#[test]
	fn test_prefix_join() {
		// Basic joining
		let prefix = Path::new("foo");
		let suffix = Path::new("bar");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar");

		// Trailing slash on prefix
		let prefix = Path::new("foo/");
		let suffix = Path::new("bar");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar");

		// Leading slash on suffix
		let prefix = Path::new("foo");
		let suffix = Path::new("/bar");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar");

		// Trailing slash on suffix
		let prefix = Path::new("foo");
		let suffix = Path::new("bar/");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar"); // trailing slash is trimmed

		// Both have slashes
		let prefix = Path::new("foo/");
		let suffix = Path::new("/bar");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar");

		// Empty suffix
		let prefix = Path::new("foo");
		let suffix = Path::new("");
		assert_eq!(prefix.join(suffix).as_str(), "foo");

		// Empty prefix
		let prefix = Path::new("");
		let suffix = Path::new("bar");
		assert_eq!(prefix.join(suffix).as_str(), "bar");

		// Both empty
		let prefix = Path::new("");
		let suffix = Path::new("");
		assert_eq!(prefix.join(suffix).as_str(), "");

		// Complex paths
		let prefix = Path::new("foo/bar");
		let suffix = Path::new("baz/qux");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar/baz/qux");

		// Complex paths with slashes
		let prefix = Path::new("foo/bar/");
		let suffix = Path::new("/baz/qux/");
		assert_eq!(prefix.join(suffix).as_str(), "foo/bar/baz/qux"); // all slashes are trimmed
	}

	#[test]
	fn test_path_ref() {
		// Test PathRef creation and normalization
		let ref1 = Path::new("/foo/bar/");
		assert_eq!(ref1.as_str(), "foo/bar");

		let ref2 = Path::from("///foo///");
		assert_eq!(ref2.as_str(), "foo");

		// Test PathRef normalizes multiple slashes
		let ref3 = Path::new("foo//bar///baz");
		assert_eq!(ref3.as_str(), "foo/bar/baz");

		// Test conversions
		let path = Path::new("foo/bar");
		let path_ref = path;
		assert_eq!(path_ref.as_str(), "foo/bar");

		// Test that Path methods work with PathRef
		let path2 = Path::new("foo/bar/baz");
		assert!(path2.has_prefix(&path_ref));
		assert_eq!(path2.strip_prefix(path_ref).unwrap().as_str(), "baz");

		// Test empty PathRef
		let empty = Path::new("");
		assert!(empty.is_empty());
		assert_eq!(empty.len(), 0);
	}

	#[test]
	fn test_multiple_consecutive_slashes() {
		let path = Path::new("foo//bar///baz");
		// Multiple consecutive slashes are collapsed to single slashes
		assert_eq!(path.as_str(), "foo/bar/baz");

		// Test with leading and trailing slashes too
		let path2 = Path::new("//foo//bar///baz//");
		assert_eq!(path2.as_str(), "foo/bar/baz");

		// Test empty segments are handled correctly
		let path3 = Path::new("foo///bar");
		assert_eq!(path3.as_str(), "foo/bar");
	}

	#[test]
	fn test_removes_multiple_slashes_comprehensively() {
		// Test various multiple slash scenarios
		assert_eq!(Path::new("foo//bar").as_str(), "foo/bar");
		assert_eq!(Path::new("foo///bar").as_str(), "foo/bar");
		assert_eq!(Path::new("foo////bar").as_str(), "foo/bar");

		// Multiple occurrences of double slashes
		assert_eq!(Path::new("foo//bar//baz").as_str(), "foo/bar/baz");
		assert_eq!(Path::new("a//b//c//d").as_str(), "a/b/c/d");

		// Mixed slash counts
		assert_eq!(Path::new("foo//bar///baz////qux").as_str(), "foo/bar/baz/qux");

		// With leading and trailing slashes
		assert_eq!(Path::new("//foo//bar//").as_str(), "foo/bar");
		assert_eq!(Path::new("///foo///bar///").as_str(), "foo/bar");

		// Edge case: only slashes
		assert_eq!(Path::new("//").as_str(), "");
		assert_eq!(Path::new("////").as_str(), "");

		// Test that operations work correctly with normalized paths
		let path_with_slashes = Path::new("foo//bar///baz");
		assert!(path_with_slashes.has_prefix("foo/bar"));
		assert_eq!(path_with_slashes.strip_prefix("foo").unwrap().as_str(), "bar/baz");
		assert_eq!(path_with_slashes.join("qux").as_str(), "foo/bar/baz/qux");

		// Test PathRef to Path conversion
		let path_ref = Path::new("foo//bar///baz");
		assert_eq!(path_ref.as_str(), "foo/bar/baz"); // PathRef now normalizes too
		let path_from_ref = path_ref.to_owned();
		assert_eq!(path_from_ref.as_str(), "foo/bar/baz"); // Both are normalized
	}

	#[test]
	fn test_path_ref_multiple_slashes() {
		// PathRef now normalizes multiple slashes using Cow
		let path_ref = Path::new("//foo//bar///baz//");
		assert_eq!(path_ref.as_str(), "foo/bar/baz"); // Fully normalized

		// Various multiple slash scenarios are normalized in PathRef
		assert_eq!(Path::new("foo//bar").as_str(), "foo/bar");
		assert_eq!(Path::new("foo///bar").as_str(), "foo/bar");
		assert_eq!(Path::new("a//b//c//d").as_str(), "a/b/c/d");

		// Conversion to Path maintains normalized form
		assert_eq!(Path::new("foo//bar").to_owned().as_str(), "foo/bar");
		assert_eq!(Path::new("foo///bar").to_owned().as_str(), "foo/bar");
		assert_eq!(Path::new("a//b//c//d").to_owned().as_str(), "a/b/c/d");

		// Edge cases
		assert_eq!(Path::new("//").as_str(), "");
		assert_eq!(Path::new("////").as_str(), "");
		assert_eq!(Path::new("//").to_owned().as_str(), "");
		assert_eq!(Path::new("////").to_owned().as_str(), "");

		// Test that PathRef avoids allocation when no normalization needed
		let normal_path = Path::new("foo/bar/baz");
		assert_eq!(normal_path.as_str(), "foo/bar/baz");
		// This should use Cow::Borrowed internally (no allocation)

		let needs_norm = Path::new("foo//bar");
		assert_eq!(needs_norm.as_str(), "foo/bar");
		// This should use Cow::Owned internally (allocation only when needed)
	}

	#[test]
	fn test_ergonomic_conversions() {
		// Test that all these work ergonomically in function calls
		fn takes_path_ref<'a>(p: impl Into<Path<'a>>) -> String {
			p.into().as_str().to_string()
		}

		// Alternative API using the trait alias for better error messages
		fn takes_path_ref_with_trait<'a>(p: impl Into<Path<'a>>) -> String {
			p.into().as_str().to_string()
		}

		// String literal
		assert_eq!(takes_path_ref("foo//bar"), "foo/bar");

		// String (owned) - this should now work without &
		let owned_string = String::from("foo//bar///baz");
		assert_eq!(takes_path_ref(owned_string), "foo/bar/baz");

		// &String
		let string_ref = String::from("foo//bar");
		assert_eq!(takes_path_ref(string_ref), "foo/bar");

		// PathRef
		let path_ref = Path::new("foo//bar");
		assert_eq!(takes_path_ref(path_ref), "foo/bar");

		// Path
		let path = Path::new("foo//bar");
		assert_eq!(takes_path_ref(path), "foo/bar");

		// Test that Path::new works with all these types
		let _path1 = Path::new("foo/bar"); // &str
		let _path2 = Path::new("foo/bar"); // String - should now work
		let _path3 = Path::new("foo/bar"); // &String
		let _path4 = Path::new("foo/bar"); // PathRef

		// Test the trait alias version works the same
		assert_eq!(takes_path_ref_with_trait("foo//bar"), "foo/bar");
		assert_eq!(takes_path_ref_with_trait(String::from("foo//bar")), "foo/bar");
	}

	#[test]
	fn test_prefix_strip_prefix() {
		// Test basic stripping
		let prefix = Path::new("foo/bar/baz");
		assert_eq!(prefix.strip_prefix("").unwrap().as_str(), "foo/bar/baz");
		assert_eq!(prefix.strip_prefix("foo").unwrap().as_str(), "bar/baz");
		assert_eq!(prefix.strip_prefix("foo/").unwrap().as_str(), "bar/baz");
		assert_eq!(prefix.strip_prefix("foo/bar").unwrap().as_str(), "baz");
		assert_eq!(prefix.strip_prefix("foo/bar/").unwrap().as_str(), "baz");
		assert_eq!(prefix.strip_prefix("foo/bar/baz").unwrap().as_str(), "");

		// Test invalid prefixes
		assert!(prefix.strip_prefix("fo").is_none());
		assert!(prefix.strip_prefix("bar").is_none());
		assert!(prefix.strip_prefix("foo/ba").is_none());

		// Test edge cases
		let prefix = Path::new("foobar");
		assert!(prefix.strip_prefix("foo").is_none());
		assert_eq!(prefix.strip_prefix("foobar").unwrap().as_str(), "");

		// Test empty prefix
		let prefix = Path::new("");
		assert_eq!(prefix.strip_prefix("").unwrap().as_str(), "");
		assert!(prefix.strip_prefix("foo").is_none());

		// Test single component
		let prefix = Path::new("foo");
		assert_eq!(prefix.strip_prefix("foo").unwrap().as_str(), "");
		assert_eq!(prefix.strip_prefix("foo/").unwrap().as_str(), ""); // "foo/" becomes "foo" after trimming

		// Test trailing slash handling
		let prefix = Path::new("foo/bar/");
		assert_eq!(prefix.strip_prefix("foo").unwrap().as_str(), "bar");
		assert_eq!(prefix.strip_prefix("foo/").unwrap().as_str(), "bar");
		assert_eq!(prefix.strip_prefix("foo/bar").unwrap().as_str(), "");
		assert_eq!(prefix.strip_prefix("foo/bar/").unwrap().as_str(), "");
	}

	#[test]
	fn test_prefix_list_dedup() {
		// Exact duplicates are removed
		let list = PathPrefixes::new(["demo", "demo"]);
		assert_eq!(list.len(), 1);
		assert_eq!(list[0], Path::new("demo"));
	}

	#[test]
	fn test_prefix_list_overlap() {
		// "demo/foo" is redundant when "demo" exists
		let list = PathPrefixes::new(["demo", "demo/foo", "anon"]);
		assert_eq!(list.len(), 2);
		assert!(list.iter().any(|p| p == &Path::new("demo")));
		assert!(list.iter().any(|p| p == &Path::new("anon")));
	}

	#[test]
	fn test_prefix_list_overlap_reverse_order() {
		// Order shouldn't matter
		let list = PathPrefixes::new(["demo/foo", "demo"]);
		assert_eq!(list.len(), 1);
		assert_eq!(list[0], Path::new("demo"));
	}

	#[test]
	fn test_prefix_list_empty_covers_all() {
		// Empty prefix covers everything
		let list = PathPrefixes::new(["", "demo", "anon"]);
		assert_eq!(list.len(), 1);
		assert_eq!(list[0], Path::new(""));
	}

	#[test]
	fn test_prefix_list_no_overlap() {
		// Unrelated prefixes are all kept
		let list = PathPrefixes::new(["demo", "anon", "secret"]);
		assert_eq!(list.len(), 3);
	}

	#[test]
	fn test_prefix_list_single() {
		let list = PathPrefixes::new(["demo"]);
		assert_eq!(list.len(), 1);
	}

	#[test]
	fn test_prefix_list_empty() {
		let list = PathPrefixes::new(std::iter::empty::<&str>());
		assert!(list.is_empty());
		assert_eq!(list.len(), 0);
	}

	#[test]
	fn test_prefix_list_deep_overlap() {
		// "a/b/c" is covered by "a/b" which is covered by "a"
		let list = PathPrefixes::new(["a/b/c", "a/b", "a"]);
		assert_eq!(list.len(), 1);
		assert_eq!(list[0], Path::new("a"));
	}

	#[test]
	fn test_prefix_list_partial_name_not_overlap() {
		// "demo" should NOT cover "demonstration" (different path component)
		let list = PathPrefixes::new(["demo", "demonstration"]);
		assert_eq!(list.len(), 2);
	}

	#[test]
	fn test_prefix_list_collect() {
		let paths: Vec<PathOwned> = vec!["demo".into(), "demo/foo".into()];
		let list: PathPrefixes = paths.into_iter().collect();
		assert_eq!(list.len(), 1);
		assert_eq!(list[0], Path::new("demo"));
	}

	#[test]
	fn test_prefix_list_eq_vec() {
		let list = PathPrefixes::new(["demo", "anon"]);
		// Canonical order: sorted by length, then lexicographically
		assert_eq!(list, vec!["anon".as_path(), "demo".as_path()]);
	}

	// Pointer-equality checks that owned paths share one allocation through the
	// clone / to_owned / strip_prefix flow used by origin announce fan-out.
	#[test]
	fn test_owned_paths_share_allocation() {
		let path = Path::new("customer/room/broadcast").to_owned();

		// Cloning an owned path shares the buffer.
		let cloned = path.clone();
		assert_eq!(path.as_str().as_ptr(), cloned.as_str().as_ptr());

		// as_path + to_owned (how notify queues a path per consumer) shares too.
		let requeued = path.as_path().to_owned();
		assert_eq!(path.as_str().as_ptr(), requeued.as_str().as_ptr());

		// Stripping a prefix from an owned path is offset arithmetic, not a copy.
		let stripped = path.strip_prefix("customer").unwrap().to_owned();
		assert_eq!(stripped.as_str(), "room/broadcast");
		assert_eq!(stripped.as_str().as_ptr(), path.as_str()["customer/".len()..].as_ptr());

		// next_part shares the rest as well.
		let (dir, rest) = path.next_part().unwrap();
		assert_eq!(dir, "customer");
		let rest = rest.to_owned();
		assert_eq!(rest.as_str().as_ptr(), stripped.as_str().as_ptr());

		// join produces an owned path whose clones share.
		let joined = path.join("alice");
		let joined2 = joined.clone();
		assert_eq!(joined.as_str(), "customer/room/broadcast/alice");
		assert_eq!(joined.as_str().as_ptr(), joined2.as_str().as_ptr());
	}

	#[test]
	fn test_parts() {
		assert_eq!(Path::empty().parts().count(), 0);
		assert_eq!(Path::new("foo").parts().collect::<Vec<_>>(), ["foo"]);
		assert_eq!(Path::new("/foo//bar/").parts().collect::<Vec<_>>(), ["foo", "bar"]);
	}

	#[test]
	fn test_wire_max_parts() {
		use crate::lite::Version;

		let ok = (0..Path::MAX_PARTS)
			.map(|i| i.to_string())
			.collect::<Vec<_>>()
			.join("/");
		let too_deep = format!("{ok}/extra");

		// Encode enforces the limit.
		let mut buf = bytes::BytesMut::new();
		Path::new(&ok).encode(&mut buf, Version::Lite04).unwrap();
		assert!(matches!(
			Path::new(&too_deep).encode(&mut bytes::BytesMut::new(), Version::Lite04),
			Err(EncodeError::BoundsExceeded)
		));

		// Decode round-trips at the limit.
		let decoded = Path::decode(&mut buf.freeze(), Version::Lite04).unwrap();
		assert_eq!(decoded.as_str(), ok);

		// Decode enforces the limit on a raw string that encode would have refused.
		let mut buf = bytes::BytesMut::new();
		too_deep.as_str().encode(&mut buf, Version::Lite04).unwrap();
		assert!(matches!(
			Path::decode(&mut buf.freeze(), Version::Lite04),
			Err(DecodeError::BoundsExceeded)
		));
	}

	#[test]
	fn test_owned_empty_paths() {
		// Empty paths never allocate and stay well-behaved.
		let empty = Path::new("").to_owned();
		assert!(empty.is_empty());
		assert_eq!(empty, Path::empty());

		let path = Path::new("foo").to_owned();
		let rest = path.strip_prefix("foo").unwrap().to_owned();
		assert!(rest.is_empty());
	}

	#[test]
	fn test_prefix_list_canonical_order() {
		// Same inputs in different order produce identical results
		let a = PathPrefixes::new(["foo", "bar"]);
		let b = PathPrefixes::new(["bar", "foo"]);
		assert_eq!(a, b);
	}

	#[test]
	fn test_path_relative_normalize() {
		assert_eq!(PathRelative::new("foo").as_str(), "foo");
		assert_eq!(PathRelative::new("/foo/").as_str(), "foo");
		assert_eq!(PathRelative::new("foo//bar").as_str(), "foo/bar");
		assert_eq!(PathRelative::new("../foo").as_str(), "../foo");
		assert_eq!(PathRelative::new("../../a/b").as_str(), "../../a/b");
		assert!(PathRelative::new("").is_empty());
	}

	#[test]
	fn test_path_relative_strips_dot_segments() {
		assert_eq!(PathRelative::new(".").as_str(), "");
		assert_eq!(PathRelative::new("./foo").as_str(), "foo");
		assert_eq!(PathRelative::new("foo/./bar").as_str(), "foo/bar");
		assert_eq!(PathRelative::new("./../foo").as_str(), "../foo");
		// From<String> takes the same normalization.
		assert_eq!(PathRelative::from("./foo".to_string()).as_str(), "foo");
		assert_eq!(PathRelative::from(".".to_string()).as_str(), "");
	}

	#[test]
	fn test_resolve_no_dotdot() {
		let base = Path::new("a/b");
		assert_eq!(base.resolve(&PathRelative::new("c")).as_str(), "a/b/c");
		assert_eq!(base.resolve(&PathRelative::new("c/d")).as_str(), "a/b/c/d");
	}

	#[test]
	fn test_resolve_empty_rel_returns_base() {
		let base = Path::new("a/b");
		assert_eq!(base.resolve(&PathRelative::new("")).as_str(), "a/b");
	}

	#[test]
	fn test_resolve_single_dotdot() {
		let base = Path::new("a/b/c");
		assert_eq!(base.resolve(&PathRelative::new("../d")).as_str(), "a/b/d");
		assert_eq!(base.resolve(&PathRelative::new("..")).as_str(), "a/b");
	}

	#[test]
	fn test_resolve_multiple_dotdot() {
		let base = Path::new("a/b/c");
		assert_eq!(base.resolve(&PathRelative::new("../../x")).as_str(), "a/x");
		assert_eq!(base.resolve(&PathRelative::new("../../../x")).as_str(), "x");
	}

	#[test]
	fn test_resolve_dotdot_clamps_at_root() {
		let base = Path::new("a");
		// Excess `..` clamps at the root instead of escaping it.
		assert_eq!(base.resolve(&PathRelative::new("../../../foo")).as_str(), "foo");
		assert_eq!(base.resolve(&PathRelative::new("..")).as_str(), "");
	}

	#[test]
	fn test_resolve_empty_base() {
		let base = Path::empty();
		assert_eq!(base.resolve(&PathRelative::new("foo")).as_str(), "foo");
		assert_eq!(base.resolve(&PathRelative::new("..")).as_str(), "");
	}

	#[test]
	fn test_resolve_dot_is_noop() {
		let base = Path::new("a/b");
		// `.` is normalized away by PathRelative::new, so resolve ignores it.
		assert_eq!(base.resolve(&PathRelative::new(".")).as_str(), "a/b");
		assert_eq!(base.resolve(&PathRelative::new("./c")).as_str(), "a/b/c");
		assert_eq!(base.resolve(&PathRelative::new("./../c")).as_str(), "a/c");
	}

	#[test]
	fn test_resolve_self_reference_via_dotdot() {
		// Walking `..` back to the same path yields the base unchanged, which lets the
		// caller compare resolved == base to detect a self-reference.
		let base = Path::new("a/b");
		assert_eq!(base.resolve(&PathRelative::new("../b")).as_str(), "a/b");
	}
}
