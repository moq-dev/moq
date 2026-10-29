//! Broadcast paths and the patterns that match them.
//!
//! [`Path`] is a literal coordinate: `/`-separated non-empty segments, with
//! segment-aware prefix operations. [`Pattern`] describes a set of paths with
//! wildcards, and [`Patterns`] is a union of them reduced by containment. The
//! grammar and algebra live in [`moq-pattern`](moq_pattern); this module
//! re-exports them beside [`Path`] so grants, origin scopes, and announce
//! interests can share one dialect.

pub use moq_pattern::{InvalidPattern, Pattern, Patterns, Segment, Specificity};

use std::fmt::{self, Display};
use std::sync::Arc;

use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

// Paths up to this many bytes live inline, in the word `Shared` holds beside its `Arc` less the
// length and the tag: 22 bytes on 64-bit targets and 10 on 32-bit ones.
const INLINE: usize = size_of::<Arc<str>>() + size_of::<usize>() - 2;

/// Inline bytes, a static literal, or a suffix of a shared reference-counted buffer.
///
/// Short paths copy inline, so cloning one is a memcpy with no atomics. A long path shares
/// one allocation: cloning bumps a refcount and suffix operations like `strip_prefix` only
/// advance `start`, so one allocation serves every copy of a path as it fans out.
#[derive(Clone)]
enum Repr {
	Inline { len: u8, buf: [u8; INLINE] },
	Static(&'static str),
	Shared { start: u16, buf: Arc<str> },
}

const _: () = assert!(size_of::<Repr>() == INLINE + 2);
// Every path fits `MAX_BYTES`, so a `Shared` offset into one always fits its `u16`.
const _: () = assert!(Path::MAX_BYTES <= u16::MAX as usize);

impl Repr {
	fn copy(s: &str) -> Result<Self, Invalid> {
		if s.len() > Path::MAX_BYTES {
			return Err(Invalid::TooLong);
		}
		Ok(Self::copy_bounded(s))
	}

	// `copy` for a string already known to fit `MAX_BYTES`.
	fn copy_bounded(s: &str) -> Self {
		debug_assert!(s.len() <= Path::MAX_BYTES);
		if s.len() <= INLINE {
			let mut buf = [0; INLINE];
			buf[..s.len()].copy_from_slice(s.as_bytes());
			Repr::Inline {
				len: s.len() as u8,
				buf,
			}
		} else {
			Repr::Shared {
				start: 0,
				buf: s.into(),
			}
		}
	}
}

/// Why a string is not a valid [`Path`] or [`Relative`].
#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum Invalid {
	/// Longer than [`Path::MAX_BYTES`].
	#[error("path is longer than {} bytes", Path::MAX_BYTES)]
	TooLong,

	/// A leading, trailing, or doubled `/`, which would give one path several spellings.
	#[error("path contains an empty segment")]
	EmptySegment,
}

// A path is refused rather than normalized, so each one has exactly one spelling.
fn validate(s: &str) -> Result<(), Invalid> {
	if s.len() > Path::MAX_BYTES {
		return Err(Invalid::TooLong);
	}
	if !s.is_empty() && (s.starts_with('/') || s.ends_with('/') || s.contains("//")) {
		return Err(Invalid::EmptySegment);
	}
	Ok(())
}

/// A broadcast path that provides safe prefix matching operations.
///
/// This type wraps a string but provides path-aware operations that respect
/// delimiter boundaries, preventing issues like "foo" matching "foobar".
///
/// A path is a sequence of non-empty segments joined by `/`, so it has one spelling:
/// [`Path::new`] refuses a leading, trailing, or doubled `/` rather than collapsing it.
/// A path is at most [`Path::MAX_BYTES`] long. The empty path has no segments; it is the
/// origin's root and prefixes every other path. The same type names an exact broadcast
/// and the prefix a route or announcement covers, so a broadcast's own path is the prefix
/// it announces, and a prefix needs no trailing `/`. See [`Relative`] for `..`-style
/// references.
///
/// Cloning is cheap: a short path is stored inline, and a long one shares a single
/// reference-counted allocation that suffix operations like [`Path::strip_prefix`] reuse.
///
/// # Examples
/// ```
/// use moq_net::Path;
///
/// // An empty segment is refused, not collapsed.
/// assert!(Path::new("/foo/bar/").is_err());
/// assert!(Path::new("foo//bar").is_err());
///
/// let base = Path::new("api/v1")?;
/// assert!(base.has_prefix(&Path::from_static("api")));
/// assert!(base.has_prefix(&Path::empty()));
///
/// let joined = base.join(&Path::from_static("users"))?;
/// assert_eq!(joined.as_str(), "api/v1/users");
/// # Ok::<(), moq_net::path::Invalid>(())
/// ```
#[derive(Clone)]
pub struct Path(Repr);

impl Path {
	/// Maximum number of slash-separated parts in a path.
	///
	/// Matches the IETF moq-transport limit of 32 fields in a namespace tuple.
	/// moq-lite enforces the same bound: encoding or decoding a deeper path fails,
	/// and publishing one to an origin is rejected.
	pub const MAX_PARTS: usize = 32;

	/// Maximum length of a path in bytes.
	///
	/// Matches the IETF moq-transport limit of 4096 bytes for a namespace. Its tuple fields
	/// drop the path's separators and escapes, so every path fits both wires.
	pub const MAX_BYTES: usize = 4096;

	/// Create a new Path by copying a string slice.
	///
	/// The empty string is the root path. Anything else must be non-empty segments joined
	/// by `/`: a leading, trailing, or doubled `/` fails with [`Invalid::EmptySegment`], and
	/// a string longer than [`Path::MAX_BYTES`] with [`Invalid::TooLong`].
	pub fn new(s: &str) -> Result<Self, Invalid> {
		validate(s)?;
		Ok(Self(Repr::copy_bounded(s)))
	}

	/// Create a new Path from a string literal without copying it, validated like [`Path::new`].
	///
	/// # Panics
	///
	/// If the literal is not a valid path.
	pub fn from_static(s: &'static str) -> Self {
		if let Err(err) = validate(s) {
			panic!("invalid path literal {s:?}: {err}");
		}
		Self(Repr::Static(s))
	}

	// A copy of this path skipping the first `n` bytes, reusing the shared buffer when possible.
	pub(crate) fn slice_from(&self, n: usize) -> Path {
		let rest = &self.as_str()[n..];
		match &self.0 {
			Repr::Static(s) => Path(Repr::Static(&s[n..])),
			Repr::Shared { buf, start } if rest.len() > INLINE => Path(Repr::Shared {
				buf: buf.clone(),
				// The offset stays within the buffer, which fits `MAX_BYTES` and so a `u16`.
				start: *start + n as u16,
			}),
			_ => Path(Repr::copy_bounded(rest)),
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
	/// let path = Path::from_static("foo/bar");
	/// assert!(path.has_prefix(&Path::from_static("foo")));
	/// assert!(!path.has_prefix(&Path::from_static("fo")));
	///
	/// let path = Path::from_static("foobar");
	/// assert!(!path.has_prefix(&Path::from_static("foo")));
	/// ```
	pub fn has_prefix(&self, prefix: &Path) -> bool {
		self.prefix_len(prefix).is_some()
	}

	// The bytes to skip past `prefix` and its delimiter, if it is a whole-segment prefix.
	fn prefix_len(&self, prefix: &Path) -> Option<usize> {
		let prefix = prefix.as_str();
		if prefix.is_empty() {
			return Some(0);
		}

		let s = self.as_str();
		if !s.starts_with(prefix) {
			return None;
		}

		// Check if the prefix is the exact match
		if s.len() == prefix.len() {
			return Some(s.len());
		}

		// Otherwise, ensure the character after the prefix is a delimiter
		(s.as_bytes().get(prefix.len()) == Some(&b'/')).then_some(prefix.len() + 1)
	}

	/// The remainder after removing `prefix`, or `None` if it isn't a prefix.
	///
	/// Only whole segments match: `a/bc` is not prefixed by `a/b`. An empty prefix
	/// returns the whole path.
	pub fn strip_prefix(&self, prefix: &Path) -> Option<Path> {
		let n = self.prefix_len(prefix)?;
		Some(self.slice_from(n))
	}

	/// The first `n` segments, or `None` if the path has fewer.
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// let path = Path::from_static("foo/bar/baz");
	/// assert_eq!(path.head(2), Some(Path::from_static("foo/bar")));
	/// assert_eq!(path.head(0), Some(Path::empty()));
	/// assert_eq!(path.head(4), None);
	/// ```
	pub fn head(&self, n: usize) -> Option<Path> {
		if n == 0 {
			return Some(Path::empty());
		}
		let s = self.as_str();
		let end = s
			.match_indices('/')
			.map(|(i, _)| i)
			.chain((!s.is_empty()).then_some(s.len()))
			.nth(n - 1)?;
		// A run of whole segments of a valid path is itself valid.
		Some(match &self.0 {
			Repr::Static(s) => Path(Repr::Static(&s[..end])),
			_ => Path(Repr::copy_bounded(&s[..end])),
		})
	}

	/// Iterate over the slash-separated parts of the path.
	///
	/// The empty path has no parts.
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// let path = Path::from_static("foo/bar/baz");
	/// assert_eq!(path.parts().collect::<Vec<_>>(), ["foo", "bar", "baz"]);
	/// assert_eq!(Path::empty().parts().count(), 0);
	/// ```
	pub fn parts(&self) -> impl Iterator<Item = &str> {
		// Segments are never empty, so only the empty path ends in an empty substring,
		// which a terminator split skips; the route table walks this on every lookup.
		self.as_str().split_terminator('/')
	}

	/// Whether a segment starts with `.`, which hides the path from announce
	/// discovery below the requested prefix.
	pub(crate) fn is_hidden(&self) -> bool {
		self.parts().any(|part| part.starts_with('.'))
	}

	/// The path as a string, with no leading or trailing slash.
	pub fn as_str(&self) -> &str {
		match &self.0 {
			// SAFETY: `Repr::copy` and `concat` fill `buf[..len]` only from whole `&str`s and an
			// ASCII `/`, so those bytes are valid UTF-8.
			Repr::Inline { len, buf } => unsafe { std::str::from_utf8_unchecked(&buf[..*len as usize]) },
			Repr::Static(s) => s,
			Repr::Shared { buf, start } => &buf[*start as usize..],
		}
	}

	/// The empty path, which prefixes every other path.
	pub const fn empty() -> Path {
		Path(Repr::Static(""))
	}

	/// Returns `true` if this is the empty path.
	pub fn is_empty(&self) -> bool {
		self.as_str().is_empty()
	}

	/// The length in bytes, not segments.
	pub fn len(&self) -> usize {
		self.as_str().len()
	}

	/// Join this path with another path component.
	///
	/// Fails if the result is longer than [`Path::MAX_BYTES`].
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// let base = Path::from_static("foo");
	/// let joined = base.join(&Path::from_static("bar"))?;
	/// assert_eq!(joined.as_str(), "foo/bar");
	///
	/// // The empty path is the identity.
	/// assert_eq!(base.join(&Path::empty())?.as_str(), "foo");
	/// # Ok::<(), moq_net::path::Invalid>(())
	/// ```
	pub fn join(&self, other: &Path) -> Result<Path, Invalid> {
		if self.is_empty() {
			Ok(other.clone())
		} else if other.is_empty() {
			Ok(self.clone())
		} else {
			Self::concat(self.as_str(), other.as_str())
		}
	}

	// `join` where the result is known to fit, like a prefix and the suffix split from
	// one path. Panics otherwise, since that breaks an internal invariant.
	pub(crate) fn rejoin(&self, other: &Path) -> Path {
		self.join(other).expect("a rejoined path fits")
	}

	// `a/b` for two non-empty valid paths, built in place.
	pub(crate) fn concat(a: &str, b: &str) -> Result<Path, Invalid> {
		let len = a.len() + 1 + b.len();
		if len > Self::MAX_BYTES {
			return Err(Invalid::TooLong);
		}
		if len <= INLINE {
			let mut buf = [0; INLINE];
			buf[..a.len()].copy_from_slice(a.as_bytes());
			buf[a.len()] = b'/';
			buf[a.len() + 1..len].copy_from_slice(b.as_bytes());
			Ok(Path(Repr::Inline { len: len as u8, buf }))
		} else {
			// Sized up front: `format!` guesses low and reallocates.
			let mut buf = String::with_capacity(len);
			buf.push_str(a);
			buf.push('/');
			buf.push_str(b);
			Ok(Path(Repr::Shared {
				buf: buf.into(),
				start: 0,
			}))
		}
	}

	/// Resolve a [`Relative`] against this path.
	///
	/// A non-empty reference replaces the last segment of the base, matching relative URL
	/// resolution. `..` segments then pop another segment; other segments are appended.
	/// Excess `..` is a no-op once the base is empty (subsequent named segments still append).
	/// An empty `rel` returns this path unchanged.
	///
	/// [`Relative::new`] strips empty and redundant `.` segments, but preserves a lone `.`
	/// so it can reference the base's parent.
	///
	/// Fails if the result is longer than [`Path::MAX_BYTES`].
	///
	/// # Examples
	/// ```
	/// use moq_net::{Path, path::Relative};
	///
	/// let base = Path::from_static("a/b/c");
	/// assert_eq!(base.resolve(&Relative::new("./d")?)?.as_str(), "a/b/d");
	/// assert_eq!(base.resolve(&Relative::new(".")?)?.as_str(), "a/b");
	/// assert_eq!(base.resolve(&Relative::new("../d")?)?.as_str(), "a/d");
	/// # Ok::<(), moq_net::path::Invalid>(())
	/// ```
	pub fn resolve(&self, rel: &Relative) -> Result<Path, Invalid> {
		if rel.is_empty() {
			return Ok(self.clone());
		}

		let mut segments: Vec<&str> = self.parts().collect();
		segments.pop();

		for seg in rel.as_str().split('/') {
			if seg == "." {
				continue;
			} else if seg == ".." {
				segments.pop();
			} else {
				segments.push(seg);
			}
		}

		Ok(Path(Repr::copy(&segments.join("/"))?))
	}

	/// Resolve a [`Relative`], returning `None` if it escapes above the root or the
	/// result is longer than [`Path::MAX_BYTES`].
	///
	/// Unlike [`Path::resolve`], this distinguishes a valid reference to the empty root
	/// path from excess `..` segments. Use it when an untrusted relative reference must
	/// not be clamped to the root.
	pub fn try_resolve(&self, rel: &Relative) -> Option<Path> {
		if rel.is_empty() {
			return Some(self.clone());
		}

		let mut segments: Vec<&str> = self.parts().collect();
		segments.pop();

		for seg in rel.as_str().split('/') {
			if seg == "." {
				continue;
			} else if seg == ".." {
				segments.pop()?;
			} else {
				segments.push(seg);
			}
		}

		Repr::copy(&segments.join("/")).ok().map(Path)
	}

	/// Express this path relative to `base`: the inverse of [`Path::resolve`].
	///
	/// The result round-trips (`base.resolve(&rel) == self`) and never walks above the
	/// root, so [`Path::try_resolve`] accepts it too.
	///
	/// A relative reference replaces the last segment of the base, matching relative URL
	/// resolution, so a target nested under the base repeats the base's own last segment.
	///
	/// The empty reference names the base itself, so that is what a self-reference returns.
	///
	/// Returns `None` for a target no reference can name: a path segment may literally be
	/// `.` or `..`, which resolution reads as navigation instead of as a name. Only the
	/// segments past the shared prefix matter, since the rest are never emitted. A
	/// reference longer than [`Path::MAX_BYTES`] can't be named either.
	///
	/// # Examples
	/// ```
	/// use moq_net::Path;
	///
	/// // The base names a broadcast, so its last segment is replaced, not descended into.
	/// let base = Path::from_static("a/b");
	/// assert_eq!(Path::from_static("a/b/c").relative(&base).unwrap().as_str(), "b/c");
	/// assert_eq!(Path::from_static("a/c").relative(&base).unwrap().as_str(), "c");
	/// assert_eq!(Path::from_static("c").relative(&base).unwrap().as_str(), "../c");
	///
	/// // The lone `.` names the base's parent, which the empty reference cannot.
	/// assert_eq!(Path::from_static("a").relative(&base).unwrap().as_str(), ".");
	///
	/// // The base itself.
	/// assert_eq!(Path::from_static("a/b").relative(&base).unwrap().as_str(), "");
	///
	/// // A segment named `..` is a legal path component but an unnameable target.
	/// assert!(Path::from_static("a/..").relative(&base).is_none());
	/// ```
	pub fn relative(&self, base: &Path) -> Option<Relative> {
		// Only the empty reference can name a base whose last segment is itself `.` or `..`,
		// since resolution replaces that segment rather than emitting it.
		if self == base {
			return Some(Relative::empty());
		}

		// Resolution replaces the base's last segment, so walk from its parent.
		let mut dir: Vec<&str> = base.parts().collect();
		dir.pop();

		let target: Vec<&str> = self.parts().collect();
		let common = dir.iter().zip(&target).take_while(|(a, b)| a == b).count();

		let down = &target[common..];
		if down.iter().any(|part| *part == "." || *part == "..") {
			// Resolution would walk on these instead of naming them.
			return None;
		}

		let mut rel: Vec<&str> = vec![".."; dir.len() - common];
		rel.extend(down);

		if rel.is_empty() {
			// An empty reference resolves to the base itself, so name the parent explicitly.
			return Some(Relative(Path::from_static(".")));
		}

		Relative::new(&rel.join("/")).ok()
	}
}

// `root/path` for logs, without building the joined path.
#[cfg(feature = "async")]
pub(crate) struct Rooted<'a>(pub &'a Path, pub &'a Path);

#[cfg(feature = "async")]
impl Display for Rooted<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match (self.0.is_empty(), self.1.is_empty()) {
			(_, true) => write!(f, "{}", self.0),
			(true, false) => write!(f, "{}", self.1),
			(false, false) => write!(f, "{}/{}", self.0, self.1),
		}
	}
}

// An entry point's path argument, failing with the entry point's own error type.
pub(crate) fn convert<E>(path: impl TryInto<Path, Error: Into<E>>) -> Result<Path, E> {
	path.try_into().map_err(Into::into)
}

// Comparisons, ordering, and hashing all go through `as_str()` so paths with the same
// content behave identically whatever their representation (e.g. as map keys).
impl PartialEq for Path {
	fn eq(&self, other: &Path) -> bool {
		self.as_str() == other.as_str()
	}
}

impl Eq for Path {}

impl PartialOrd for Path {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Path {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.as_str().cmp(other.as_str())
	}
}

impl std::hash::Hash for Path {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.as_str().hash(state)
	}
}

/// Look up a `Path`-keyed map by `&str` without building a `Path`. The key must be a valid path.
impl std::borrow::Borrow<str> for Path {
	fn borrow(&self) -> &str {
		self.as_str()
	}
}

impl fmt::Debug for Path {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Path").field(&self.as_str()).finish()
	}
}

impl serde::Serialize for Path {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl TryFrom<&str> for Path {
	type Error = Invalid;

	fn try_from(s: &str) -> Result<Self, Invalid> {
		Self::new(s)
	}
}

impl TryFrom<&String> for Path {
	type Error = Invalid;

	fn try_from(s: &String) -> Result<Self, Invalid> {
		Self::new(s)
	}
}

impl From<&Path> for Path {
	fn from(p: &Path) -> Self {
		p.clone()
	}
}

impl Default for Path {
	fn default() -> Self {
		Path::empty()
	}
}

impl TryFrom<String> for Path {
	type Error = Invalid;

	fn try_from(s: String) -> Result<Self, Invalid> {
		// `Arc<str>` can't adopt a `String`'s buffer, so this copies like `new`.
		Self::new(&s)
	}
}

impl AsRef<str> for Path {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

impl Display for Path {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.as_str())
	}
}

impl From<Invalid> for DecodeError {
	fn from(err: Invalid) -> Self {
		match err {
			Invalid::TooLong => DecodeError::BoundsExceeded,
			Invalid::EmptySegment => DecodeError::InvalidValue,
		}
	}
}

impl From<Invalid> for crate::Error {
	fn from(err: Invalid) -> Self {
		match err {
			Invalid::TooLong => crate::Error::BoundsExceeded(crate::BoundsExceeded),
			Invalid::EmptySegment => crate::Error::EmptySegment,
		}
	}
}

impl<'a, V> Decode<'a, V> for Path {
	fn decode(r: &mut Decoder<'a>, _: V) -> Result<Self, DecodeError> {
		// Borrowed from the wire bytes, so the path's own copy is the only one.
		let path = Path::new(r.str()?)?;
		if path.parts().count() > Path::MAX_PARTS {
			return Err(DecodeError::BoundsExceeded);
		}
		Ok(path)
	}
}

impl<V> Encode<V> for Path {
	fn encode(&self, w: &mut Encoder<'_>, _: V) -> Result<(), EncodeError> {
		if self.parts().count() > Path::MAX_PARTS {
			return Err(EncodeError::BoundsExceeded);
		}
		w.string(self.as_str())
	}
}

/// A grant's patterns as a count and each one's canonical text, the one encoding both
/// wires' AUTH_OK share.
impl<V> Encode<V> for Patterns {
	fn encode(&self, w: &mut Encoder<'_>, _: V) -> Result<(), EncodeError> {
		w.varint(self.len() as u64)?;
		for pattern in self {
			w.string(pattern.as_str())?;
		}
		Ok(())
	}
}

impl<'a, V> Decode<'a, V> for Patterns {
	fn decode(r: &mut Decoder<'a>, _: V) -> Result<Self, DecodeError> {
		let count = r.varint()?;
		// No preallocation: the count is peer-controlled, and the message size limit
		// is what bounds how many patterns actually fit.
		let mut patterns = Vec::new();
		for _ in 0..count {
			let text = r.string()?;
			let pattern = Pattern::try_from(text.as_str()).map_err(|_| DecodeError::InvalidValue)?;
			// Only the canonical spelling is valid, so each pattern has one encoding.
			if pattern.as_str() != text {
				return Err(DecodeError::InvalidValue);
			}
			patterns.push(pattern);
		}
		// Reduce once, so a peer's count never turns into quadratic work.
		Ok(patterns.into_iter().collect())
	}
}

/// A relative broadcast path, used to reference one broadcast from another broadcast's content.
///
/// Unlike [`Path`] (which is a complete reference within the broadcast namespace),
/// `Relative` may contain `.` and `..` segments to walk the namespace and is meaningful
/// only when resolved against a base [`Path`] via [`Path::resolve`]. The hang catalog uses it
/// to point a rendition at a track published in a sibling broadcast (e.g. `./source`).
///
/// `Relative` has no `Encode`/`Decode` impl, so it never appears in announce/subscribe
/// frames. It does serialize via serde for off-wire use (e.g. as a field inside a catalog
/// JSON payload, which itself travels as a track).
///
/// Normalization on creation: leading/trailing slashes are trimmed, consecutive internal
/// slashes collapse to one, and redundant `.` segments are stripped. A reference made only
/// of `.` segments normalizes to `.` rather than empty because `.` resolves to the base's
/// parent while empty resolves to the base itself. `..` is preserved for resolve time.
///
/// # Examples
/// ```
/// use moq_net::{Path, path::Relative};
///
/// let rel = Relative::new("./source")?;
/// assert_eq!(Path::from_static("a/b").resolve(&rel)?.as_str(), "a/source");
///
/// // Redundant `.` segments are stripped on creation.
/// assert_eq!(Relative::new("./a/./b")?.as_str(), "a/b");
/// assert_eq!(Relative::new(".")?.as_str(), ".");
/// # Ok::<(), moq_net::path::Invalid>(())
/// ```
#[derive(Debug, PartialEq, Eq, Hash, Clone, Default)]
pub struct Relative(Path);

impl serde::Serialize for Relative {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		self.0.serialize(serializer)
	}
}

impl Relative {
	/// Create a new `Relative` from a string slice.
	///
	/// Leading and trailing slashes are trimmed, consecutive internal slashes collapse to one,
	/// and redundant `.` segments are stripped. See the type-level doc for the full rules.
	/// Fails with [`Invalid::TooLong`] if the normalized reference is longer than [`Path::MAX_BYTES`].
	pub fn new(s: &str) -> Result<Self, Invalid> {
		let trimmed = s.trim_start_matches('/').trim_end_matches('/');

		if needs_normalize_relative(trimmed) {
			Ok(Self(Path(Repr::copy(&normalize_relative_segments(trimmed))?)))
		} else {
			Ok(Self(Path(Repr::copy(trimmed)?)))
		}
	}

	/// The normalized path as a string slice.
	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}

	/// The empty relative path, which resolves to the base path itself.
	pub const fn empty() -> Relative {
		Relative(Path::empty())
	}

	/// True if the path is empty (resolves to the base path itself).
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// The length of the normalized path in bytes.
	pub fn len(&self) -> usize {
		self.0.len()
	}
}

impl TryFrom<&str> for Relative {
	type Error = Invalid;

	fn try_from(s: &str) -> Result<Self, Invalid> {
		Self::new(s)
	}
}

impl TryFrom<&String> for Relative {
	type Error = Invalid;

	fn try_from(s: &String) -> Result<Self, Invalid> {
		Self::new(s)
	}
}

impl TryFrom<String> for Relative {
	type Error = Invalid;

	fn try_from(s: String) -> Result<Self, Invalid> {
		Self::new(&s)
	}
}

fn needs_normalize_relative(trimmed: &str) -> bool {
	trimmed.split('/').any(|seg| seg.is_empty() || seg == ".")
}

fn normalize_relative_segments(trimmed: &str) -> String {
	let segments = trimmed
		.split('/')
		.filter(|seg| !seg.is_empty() && *seg != ".")
		.collect::<Vec<_>>()
		.join("/");

	if segments.is_empty() && trimmed.split('/').any(|seg| seg == ".") {
		".".to_string()
	} else {
		segments
	}
}

impl AsRef<str> for Relative {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

impl Display for Relative {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.as_str())
	}
}

impl<'de> serde::Deserialize<'de> for Relative {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let s = String::deserialize(deserializer)?;
		Relative::new(&s).map_err(serde::de::Error::custom)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn p(s: &str) -> Path {
		Path::new(s).unwrap()
	}

	#[test]
	fn test_refuses_empty_segments() {
		for s in ["/", "//", "/foo", "foo/", "/foo/", "foo//bar", "foo/bar/", "a///b"] {
			assert_eq!(Path::new(s), Err(Invalid::EmptySegment), "accepted {s:?}");
			assert_eq!(
				Path::try_from(s.to_string()),
				Err(Invalid::EmptySegment),
				"accepted owned {s:?}"
			);
		}

		// The root has no segments, so none of them is empty.
		assert!(Path::new("").unwrap().is_empty());
		assert_eq!(Path::try_from(String::new()).unwrap(), Path::empty());

		// Every constructor agrees on everything else.
		for s in ["foo", "foo/bar", ".hidden/x", "a/../b"] {
			assert_eq!(p(s).as_str(), s);
			assert_eq!(p(s), Path::try_from(s.to_string()).unwrap());
		}
	}

	#[test]
	#[should_panic(expected = "empty segment")]
	fn test_from_static_refuses_empty_segments() {
		Path::from_static("a//b");
	}

	#[test]
	fn test_decode_refuses_empty_segments() {
		use crate::lite::Version;

		for s in ["/foo", "foo/", "foo//bar"] {
			let mut buf = Vec::new();
			Encoder::new(&mut buf, Version::Lite04.into()).string(s).unwrap();
			assert!(matches!(
				crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, |r, v| Path::decode(r, v)),
				Err(DecodeError::InvalidValue)
			));
		}
	}

	#[test]
	fn test_has_prefix() {
		let path = p("foo/bar/baz");

		assert!(path.has_prefix(&Path::empty()));
		assert!(path.has_prefix(&p("foo")));
		assert!(path.has_prefix(&p("foo/bar")));
		assert!(path.has_prefix(&p("foo/bar/baz")));

		// Only whole segments match.
		assert!(!path.has_prefix(&p("f")));
		assert!(!path.has_prefix(&p("fo")));
		assert!(!path.has_prefix(&p("foo/b")));
		assert!(!path.has_prefix(&p("foo/ba")));
		assert!(!path.has_prefix(&p("foo/bar/ba")));
		assert!(!p("foobar").has_prefix(&p("foo")));
		assert!(p("foobar").has_prefix(&p("foobar")));

		// The root prefixes everything, and is prefixed only by itself.
		assert!(Path::empty().has_prefix(&Path::empty()));
		assert!(!Path::empty().has_prefix(&p("foo")));
	}

	#[test]
	fn test_strip_prefix() {
		let path = p("foo/bar/baz");

		assert_eq!(path.strip_prefix(&Path::empty()).unwrap().as_str(), "foo/bar/baz");
		assert_eq!(path.strip_prefix(&p("foo")).unwrap().as_str(), "bar/baz");
		assert_eq!(path.strip_prefix(&p("foo/bar")).unwrap().as_str(), "baz");
		assert_eq!(path.strip_prefix(&p("foo/bar/baz")).unwrap().as_str(), "");

		assert!(path.strip_prefix(&p("fo")).is_none());
		assert!(path.strip_prefix(&p("bar")).is_none());
		assert!(path.strip_prefix(&p("foo/ba")).is_none());
		assert!(p("foobar").strip_prefix(&p("foo")).is_none());

		assert_eq!(Path::empty().strip_prefix(&Path::empty()).unwrap().as_str(), "");
		assert!(Path::empty().strip_prefix(&p("foo")).is_none());
	}

	#[test]
	fn test_head() {
		let path = p("foo/bar/baz");
		assert_eq!(path.head(1), Some(p("foo")));
		assert_eq!(path.head(3), Some(path.clone()));
		assert_eq!(path.head(4), None);
		assert_eq!(Path::empty().head(0), Some(Path::empty()));
		assert_eq!(Path::empty().head(1), None);

		// A long path's head is a valid path of its own.
		let long = p(&format!("{}/tail", "a".repeat(INLINE + 1)));
		assert_eq!(long.head(1).unwrap().as_str(), "a".repeat(INLINE + 1));
	}

	#[test]
	fn test_join() {
		assert_eq!(p("foo").join(&p("bar")).unwrap().as_str(), "foo/bar");
		assert_eq!(p("foo/bar").join(&p("baz/qux")).unwrap().as_str(), "foo/bar/baz/qux");

		// The root is the identity on either side.
		assert_eq!(p("foo").join(&Path::empty()).unwrap().as_str(), "foo");
		assert_eq!(Path::empty().join(&p("bar")).unwrap().as_str(), "bar");
		assert_eq!(Path::empty().join(&Path::empty()).unwrap().as_str(), "");
	}

	// A long path shares one allocation through the clone and strip_prefix flow used by
	// announce fan-out. A short one is stored inline instead, so its copies share nothing.
	#[test]
	fn test_long_paths_share_allocation() {
		let path = Path::new("customer/room-0123456789/broadcast").unwrap();

		// Cloning shares the buffer.
		let cloned = path.clone();
		assert_eq!(path.as_str().as_ptr(), cloned.as_str().as_ptr());

		// Stripping a prefix leaving a long rest is offset arithmetic, not a copy.
		let stripped = path.strip_prefix(&Path::from_static("customer")).unwrap();
		assert_eq!(stripped.as_str(), "room-0123456789/broadcast");
		assert_eq!(stripped.as_str().as_ptr(), path.as_str()["customer/".len()..].as_ptr());

		// A short rest moves inline rather than keeping the long buffer alive.
		let short = path
			.strip_prefix(&Path::from_static("customer/room-0123456789"))
			.unwrap();
		assert_eq!(short.as_str(), "broadcast");
		assert!(matches!(short.0, Repr::Inline { .. }));

		// join produces a path whose clones share.
		let joined = path.join(&Path::from_static("alice")).unwrap();
		let joined2 = joined.clone();
		assert_eq!(joined.as_str(), "customer/room-0123456789/broadcast/alice");
		assert_eq!(joined.as_str().as_ptr(), joined2.as_str().as_ptr());
	}

	#[test]
	fn test_inline_boundary() {
		let fits = "a".repeat(INLINE);
		let spills = "a".repeat(INLINE + 1);
		assert!(matches!(Path::new(&fits).unwrap().0, Repr::Inline { .. }));
		assert!(matches!(Path::new(&spills).unwrap().0, Repr::Shared { .. }));

		// A join exactly at the limit stays inline; one byte more spills.
		let half = Path::new(&"b".repeat(INLINE / 2 - 1)).unwrap();
		let joined = half.join(&Path::new(&"c".repeat(INLINE / 2)).unwrap()).unwrap();
		assert_eq!(joined.len(), INLINE);
		assert!(matches!(joined.0, Repr::Inline { .. }));
		assert_eq!(
			joined.as_str(),
			format!("{}/{}", "b".repeat(INLINE / 2 - 1), "c".repeat(INLINE / 2))
		);
		assert!(matches!(
			joined.join(&Path::from_static("d")).unwrap().0,
			Repr::Shared { .. }
		));

		// Multi-byte characters survive the inline copy.
		let wide = Path::new("caf\u{e9}/\u{1f3a5}").unwrap();
		assert!(matches!(wide.0, Repr::Inline { .. }));
		assert_eq!(wide.as_str(), "caf\u{e9}/\u{1f3a5}");
		assert_eq!(
			wide.strip_prefix(&Path::from_static("caf\u{e9}")).unwrap().as_str(),
			"\u{1f3a5}"
		);
	}

	#[test]
	fn test_max_bytes() {
		let longest = "a".repeat(Path::MAX_BYTES);
		let longer = format!("{longest}b");

		// Every constructor accepts the limit and refuses one byte more.
		assert_eq!(Path::new(&longest).unwrap().len(), Path::MAX_BYTES);
		assert_eq!(Path::new(&longer), Err(Invalid::TooLong));
		assert_eq!(Path::try_from(longer.clone()), Err(Invalid::TooLong));
		assert_eq!(Relative::new(&longer), Err(Invalid::TooLong));

		// So do joins and resolution, which can outgrow both sides.
		let half = Path::new(&"a".repeat(Path::MAX_BYTES / 2)).unwrap();
		let rest = Path::new(&"b".repeat(Path::MAX_BYTES / 2 - 1)).unwrap();
		assert_eq!(half.join(&rest).unwrap().len(), Path::MAX_BYTES);
		assert_eq!(
			half.join(&Path::new(&format!("{rest}b")).unwrap()),
			Err(Invalid::TooLong)
		);
		let rel = Relative::new(&format!("x/{rest}b")).unwrap();
		assert_eq!(
			Path::new(&format!("{half}/y")).unwrap().resolve(&rel),
			Err(Invalid::TooLong)
		);
		assert_eq!(Path::new(&format!("{half}/y")).unwrap().try_resolve(&rel), None);

		// A suffix at the deepest offset still shares the buffer.
		let tail = "c".repeat(INLINE + 1);
		let head = "a".repeat(Path::MAX_BYTES - tail.len() - 1);
		let path = Path::new(&format!("{head}/{tail}")).unwrap();
		let at = path.strip_prefix(&Path::new(&head).unwrap()).unwrap();
		assert_eq!(at.as_str(), tail);
		assert_eq!(at.as_str().as_ptr(), path.as_str()[head.len() + 1..].as_ptr());
	}

	#[test]
	#[should_panic(expected = "longer than")]
	fn test_from_static_too_long() {
		let literal: &'static str = Box::leak("a".repeat(Path::MAX_BYTES + 1).into_boxed_str());
		Path::from_static(literal);
	}

	#[test]
	fn test_from_static_borrows() {
		const LITERAL: &str = "static/path/that/is/longer/than/inline";
		let path = Path::from_static(LITERAL);
		assert_eq!(path.as_str(), LITERAL);
		assert_eq!(path.as_str().as_ptr(), LITERAL.as_ptr());

		// A suffix or head of a static path borrows the literal too.
		let rest = path.strip_prefix(&Path::from_static("static")).unwrap();
		assert_eq!(rest.as_str().as_ptr(), LITERAL["static/".len()..].as_ptr());
		assert_eq!(path.head(2).unwrap().as_str().as_ptr(), LITERAL.as_ptr());
	}

	#[test]
	fn test_parts() {
		assert_eq!(Path::empty().parts().count(), 0);
		assert_eq!(Path::from_static("foo").parts().collect::<Vec<_>>(), ["foo"]);
		assert_eq!(Path::from_static("foo/bar").parts().collect::<Vec<_>>(), ["foo", "bar"]);
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
		let mut buf = Vec::new();
		Path::new(&ok)
			.unwrap()
			.encode(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
			.unwrap();
		assert!(matches!(
			Path::new(&too_deep).unwrap().encode_bytes(Version::Lite04),
			Err(EncodeError::BoundsExceeded)
		));

		// Decode round-trips at the limit.
		let decoded =
			crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, |r, v| Path::decode(r, v))
				.unwrap();
		assert_eq!(decoded.as_str(), ok);

		// Decode refuses a raw string longer than the limit.
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite04.into())
			.string(too_deep.as_str())
			.unwrap();
		assert!(matches!(
			crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, |r, v| Path::decode(r, v)),
			Err(DecodeError::BoundsExceeded)
		));
	}

	#[test]
	fn test_wire_max_len() {
		use crate::lite::Version;

		let longest = "a".repeat(Path::MAX_BYTES);
		let longer = format!("{longest}b");

		// Decode round-trips at the limit.
		let mut buf = Path::new(&longest).unwrap().encode_bytes(Version::Lite04).unwrap();
		let decoded = crate::coding::decode_buf(&mut buf, Version::Lite04, |r, v| Path::decode(r, v)).unwrap();
		assert_eq!(decoded.as_str(), longest);

		// A message carrying the longest path still fits the lite message limit.
		let tracks = crate::lite::Tracks {
			broadcast: Path::new(&longest).unwrap(),
			epoch: None,
		};
		let buf = tracks.encode_bytes(Version::Lite07).unwrap();
		let (decoded, _) = crate::lite::Tracks::decode_slice(&buf, Version::Lite07).unwrap();
		assert_eq!(decoded.broadcast.as_str(), longest);

		// Decode refuses a raw string longer than the limit.
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite04.into()).string(&longer).unwrap();
		assert!(matches!(
			crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, |r, v| Path::decode(r, v)),
			Err(DecodeError::BoundsExceeded)
		));
	}

	#[test]
	fn test_owned_empty_paths() {
		// Empty paths never allocate and stay well-behaved.
		let empty = Path::from_static("");
		assert!(empty.is_empty());
		assert_eq!(empty, Path::empty());

		let path = Path::from_static("foo");
		let rest = path.strip_prefix(&Path::from_static("foo")).unwrap();
		assert!(rest.is_empty());
	}

	#[test]
	fn test_path_relative_normalize() {
		assert_eq!(Relative::new("foo").unwrap().as_str(), "foo");
		assert_eq!(Relative::new("/foo/").unwrap().as_str(), "foo");
		assert_eq!(Relative::new("foo//bar").unwrap().as_str(), "foo/bar");
		assert_eq!(Relative::new("../foo").unwrap().as_str(), "../foo");
		assert_eq!(Relative::new("../../a/b").unwrap().as_str(), "../../a/b");
		assert!(Relative::new("").unwrap().is_empty());
	}

	#[test]
	fn test_path_relative_normalizes_dot_segments() {
		assert_eq!(Relative::new(".").unwrap().as_str(), ".");
		assert_eq!(Relative::new("././").unwrap().as_str(), ".");
		assert_eq!(Relative::new("./foo").unwrap().as_str(), "foo");
		assert_eq!(Relative::new("foo/./bar").unwrap().as_str(), "foo/bar");
		assert_eq!(Relative::new("./../foo").unwrap().as_str(), "../foo");
		// TryFrom<String> takes the same normalization.
		assert_eq!(Relative::try_from("./foo".to_string()).unwrap().as_str(), "foo");
		assert_eq!(Relative::try_from(".".to_string()).unwrap().as_str(), ".");
	}

	#[test]
	fn test_resolve_replaces_base_name() {
		let base = Path::from_static("a/b");
		assert_eq!(base.resolve(&Relative::new("c").unwrap()).unwrap().as_str(), "a/c");
		assert_eq!(base.resolve(&Relative::new("c/d").unwrap()).unwrap().as_str(), "a/c/d");
		assert_eq!(
			Path::from_static("foo.hang/catalog.pro")
				.resolve(&Relative::new("./transcode.pro").unwrap())
				.unwrap()
				.as_str(),
			"foo.hang/transcode.pro"
		);
	}

	#[test]
	fn test_resolve_empty_rel_returns_base() {
		let base = Path::from_static("a/b");
		assert_eq!(base.resolve(&Relative::new("").unwrap()).unwrap().as_str(), "a/b");
	}

	#[test]
	fn test_resolve_single_dotdot() {
		let base = Path::from_static("a/b/c");
		assert_eq!(base.resolve(&Relative::new("../d").unwrap()).unwrap().as_str(), "a/d");
		assert_eq!(base.resolve(&Relative::new("..").unwrap()).unwrap().as_str(), "a");
	}

	#[test]
	fn test_resolve_multiple_dotdot() {
		let base = Path::from_static("a/b/c");
		assert_eq!(base.resolve(&Relative::new("../../x").unwrap()).unwrap().as_str(), "x");
		assert_eq!(
			base.resolve(&Relative::new("../../../x").unwrap()).unwrap().as_str(),
			"x"
		);
	}

	#[test]
	fn test_resolve_dotdot_clamps_at_root() {
		let base = Path::from_static("a");
		// Excess `..` clamps at the root instead of escaping it.
		assert_eq!(
			base.resolve(&Relative::new("../../../foo").unwrap()).unwrap().as_str(),
			"foo"
		);
		assert_eq!(base.resolve(&Relative::new("..").unwrap()).unwrap().as_str(), "");
	}

	#[test]
	fn test_resolve_empty_base() {
		let base = Path::empty();
		assert_eq!(base.resolve(&Relative::new("foo").unwrap()).unwrap().as_str(), "foo");
		assert_eq!(base.resolve(&Relative::new("..").unwrap()).unwrap().as_str(), "");
	}

	#[test]
	fn test_resolve_dot_names_parent() {
		let base = Path::from_static("a/b");
		assert_eq!(base.resolve(&Relative::new(".").unwrap()).unwrap().as_str(), "a");
		assert_eq!(base.resolve(&Relative::new("./c").unwrap()).unwrap().as_str(), "a/c");
		assert_eq!(base.resolve(&Relative::new("./../c").unwrap()).unwrap().as_str(), "c");
	}

	#[test]
	fn test_resolve_self_reference_via_sibling_name() {
		// Naming the base within its parent yields the base unchanged, which lets the
		// caller compare resolved == base to detect a self-reference.
		let base = Path::from_static("a/b");
		assert_eq!(base.resolve(&Relative::new("./b").unwrap()).unwrap().as_str(), "a/b");
	}

	#[test]
	fn test_try_resolve_distinguishes_root_from_escape() {
		let base = Path::from_static("top");
		assert_eq!(base.try_resolve(&Relative::new(".").unwrap()).unwrap().as_str(), "");
		assert!(base.try_resolve(&Relative::new("..").unwrap()).is_none());

		let nested = Path::from_static("a/b");
		assert_eq!(nested.try_resolve(&Relative::new("..").unwrap()).unwrap().as_str(), "");
		assert!(nested.try_resolve(&Relative::new("../..").unwrap()).is_none());
	}

	#[test]
	fn test_relative() {
		let rel = |target: &str, base: &str| Path::new(target).unwrap().relative(&Path::new(base).unwrap()).unwrap();

		// Nested under the base: the base's own last segment is replaced, so it repeats.
		assert_eq!(rel("foo/bar/baz", "foo/bar").as_str(), "bar/baz");
		// Sibling.
		assert_eq!(rel("foo/baz", "foo/bar").as_str(), "baz");
		// Different subtree.
		assert_eq!(rel("foo/baz/bar", "foo/bar/baz").as_str(), "../baz/bar");
		// The base's parent, which only `.` can name.
		assert_eq!(rel("a/b", "a/b/transcode.hang").as_str(), ".");
		assert_eq!(rel("a/b", "a/b/one/two/transcode.hang").as_str(), "../..");
		// Roots.
		assert_eq!(rel("foo/bar", "").as_str(), "foo/bar");
		assert_eq!(rel("", "foo").as_str(), ".");
		// The base itself, which only the empty reference names.
		assert_eq!(rel("a/b", "a/b").as_str(), "");
		assert_eq!(rel("", "").as_str(), "");
	}

	#[test]
	fn test_relative_rejects_unnameable_targets() {
		// A segment literally named `.` or `..` is a legal path component, but resolution
		// would walk on it instead of naming it.
		assert!(Path::from_static("a/../b").relative(&Path::from_static("")).is_none());
		assert!(Path::from_static("x/./y").relative(&Path::from_static("x/z")).is_none());
		assert!(Path::from_static("a/..").relative(&Path::from_static("a/b")).is_none());

		// A base is always nameable by itself, however its last segment is spelled.
		assert_eq!(
			Path::from_static("a/..")
				.relative(&Path::from_static("a/.."))
				.unwrap()
				.as_str(),
			""
		);

		// Dot segments inside the shared prefix are never emitted, so they are fine.
		let rel = Path::from_static("a/../b/x")
			.relative(&Path::from_static("a/../b/c"))
			.unwrap();
		assert_eq!(rel.as_str(), "x");
		assert_eq!(
			Path::from_static("a/../b/c").resolve(&rel).unwrap().as_str(),
			"a/../b/x"
		);
	}

	#[test]
	fn test_relative_round_trips() {
		let paths = [
			"", "a", "b", "a/b", "a/c", "a/b/c", "a/b/c/d", "x/y/z", "a/../b", "a/./b", "a/..", "a/.",
		];

		for base in paths {
			for target in paths {
				let base = Path::new(base).unwrap();
				let target = Path::new(target).unwrap();
				let Some(rel) = target.relative(&base) else {
					// Only an unnameable target may be refused, and never the base itself.
					assert!(
						target != base && target.parts().any(|part| part == "." || part == ".."),
						"{base} -> {target} refused a nameable target"
					);
					continue;
				};

				assert_eq!(base.resolve(&rel), Ok(target.clone()), "{base} -> {target} via {rel}");
				// The reference is derived from a real target, so it never escapes the root.
				assert!(
					base.try_resolve(&rel).is_some(),
					"{base} -> {target} via {rel} escaped the root"
				);
			}
		}
	}
}
