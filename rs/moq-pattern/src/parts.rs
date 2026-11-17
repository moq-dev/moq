//! The one walk that splits a broadcast path into segments.
//!
//! A `\` escapes the `\` or `/` after it, so a segment is the text between two `/` that no
//! escape covers: `a\/b` is one segment holding a `/`. Segments are yielded as written, escapes
//! included; [`unescape`] reads one's content.

use std::borrow::Cow;

/// The segments of `path`, split only at a `/` no escape covers and still escaped as written.
///
/// The empty path has none. Anything else yields one more segment than it has separators,
/// empty ones included, so a caller that requires non-empty segments checks them itself.
///
/// ```
/// let parts: Vec<_> = moq_pattern::parts(r"room/a\/b/cam").collect();
/// assert_eq!(parts, ["room", r"a\/b", "cam"]);
/// assert_eq!(moq_pattern::parts("").count(), 0);
/// ```
pub fn parts(path: &str) -> Parts<'_> {
	Parts {
		rest: (!path.is_empty()).then_some(path),
	}
}

/// The iterator [`parts`] returns, walking from either end.
#[derive(Clone, Debug)]
pub struct Parts<'a> {
	rest: Option<&'a str>,
}

// Whether the `/` at `at` is escaped: an odd run of `\` before it. `/` and `\` are ASCII, so
// the bytes before a separator never sit inside a multi-byte character. A path without `\`
// pays one byte compare per separator.
fn escaped(bytes: &[u8], at: usize) -> bool {
	bytes[..at].iter().rev().take_while(|&&b| b == b'\\').count() % 2 == 1
}

impl<'a> Iterator for Parts<'a> {
	type Item = &'a str;

	fn next(&mut self) -> Option<&'a str> {
		let s = self.rest?;
		let bytes = s.as_bytes();
		let mut from = 0;
		while let Some(i) = s[from..].find('/') {
			let at = from + i;
			if !escaped(bytes, at) {
				self.rest = Some(&s[at + 1..]);
				return Some(&s[..at]);
			}
			from = at + 1;
		}
		self.rest = None;
		Some(s)
	}
}

impl DoubleEndedIterator for Parts<'_> {
	fn next_back(&mut self) -> Option<Self::Item> {
		let s = self.rest?;
		let bytes = s.as_bytes();
		let mut to = s.len();
		while let Some(at) = s[..to].rfind('/') {
			if !escaped(bytes, at) {
				self.rest = Some(&s[..at]);
				return Some(&s[at + 1..]);
			}
			to = at;
		}
		self.rest = None;
		Some(s)
	}
}

impl std::iter::FusedIterator for Parts<'_> {}

/// A segment's content: each escaped `\` or `/` as the byte itself.
///
/// Borrows a segment with no escape. Expects a segment as [`parts`] yields it from a valid
/// path, where every `\` escapes the byte after it.
///
/// ```
/// assert_eq!(moq_pattern::unescape(r"a\/b\\c"), r"a/b\c");
/// assert_eq!(moq_pattern::unescape("plain"), "plain");
/// ```
pub fn unescape(segment: &str) -> Cow<'_, str> {
	if !segment.contains('\\') {
		return Cow::Borrowed(segment);
	}
	let mut out = String::with_capacity(segment.len());
	let mut chars = segment.chars();
	while let Some(c) = chars.next() {
		match c {
			'\\' => out.extend(chars.next()),
			c => out.push(c),
		}
	}
	Cow::Owned(out)
}

/// The segment holding `content`: each `\` or `/` escaped by a `\`. The inverse of [`unescape`].
///
/// Borrows content with neither.
///
/// ```
/// assert_eq!(moq_pattern::escape(r"a/b\c"), r"a\/b\\c");
/// ```
pub fn escape(content: &str) -> Cow<'_, str> {
	if !content.contains(['\\', '/']) {
		return Cow::Borrowed(content);
	}
	let mut out = String::with_capacity(content.len() + 2);
	for c in content.chars() {
		if matches!(c, '\\' | '/') {
			out.push('\\');
		}
		out.push(c);
	}
	Cow::Owned(out)
}

/// Whether `segment` is one escaped segment: every `\` escapes a `\` or `/`, and no `/` is bare.
pub(crate) fn is_segment(segment: &str) -> bool {
	let mut bytes = segment.bytes();
	while let Some(b) = bytes.next() {
		match b {
			b'\\' if matches!(bytes.next(), Some(b'\\' | b'/')) => {}
			b'\\' | b'/' => return false,
			_ => {}
		}
	}
	true
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn splits_only_at_unescaped_separators() {
		for (path, want) in [
			("", &[][..]),
			("a", &["a"]),
			("a/b/c", &["a", "b", "c"]),
			(r"a\/b", &[r"a\/b"]),
			(r"a\\/b", &[r"a\\", "b"]),
			(r"a\\\/b", &[r"a\\\/b"]),
			(r"\/", &[r"\/"]),
			(r"\//\/", &[r"\/", r"\/"]),
			(r"x\/\/y/z\\", &[r"x\/\/y", r"z\\"]),
			// Empty segments are yielded, not dropped: validation is the caller's.
			("/a//b/", &["", "a", "", "b", ""]),
		] {
			let forward: Vec<_> = parts(path).collect();
			assert_eq!(forward, want, "{path:?}");
			let mut backward: Vec<_> = parts(path).rev().collect();
			backward.reverse();
			assert_eq!(backward, want, "{path:?} from the back");
		}
	}

	#[test]
	fn walks_from_both_ends_at_once() {
		let mut walk = parts(r"a/b\/c/d/e");
		assert_eq!(walk.next(), Some("a"));
		assert_eq!(walk.next_back(), Some("e"));
		assert_eq!(walk.next(), Some(r"b\/c"));
		assert_eq!(walk.next_back(), Some("d"));
		assert_eq!(walk.next(), None);
		assert_eq!(walk.next_back(), None);
	}

	#[test]
	fn rejoining_the_parts_gives_back_the_path() {
		let mut texts = vec![String::new()];
		for _ in 0..8 {
			texts = texts
				.iter()
				.flat_map(|text| ['a', '/', '\\'].map(|c| format!("{text}{c}")))
				.collect();
			for text in &texts {
				let joined = parts(text).collect::<Vec<_>>().join("/");
				assert_eq!(&joined, text);
			}
		}
	}

	#[test]
	fn unescapes_content() {
		assert_eq!(unescape(r"\\"), r"\");
		assert_eq!(unescape(r"\/"), "/");
		assert_eq!(unescape("caf\u{e9}\\/\\\\x"), "caf\u{e9}/\\x");
		assert!(matches!(unescape("plain"), Cow::Borrowed("plain")));
		assert!(matches!(escape("plain"), Cow::Borrowed("plain")));
		for content in ["", "/", r"\", r"a/\b", r"\\//", "caf\u{e9}/"] {
			let segment = escape(content);
			assert!(is_segment(&segment), "{segment:?}");
			assert_eq!(parts(&segment).count(), usize::from(!content.is_empty()));
			assert_eq!(unescape(&segment), content);
		}
	}

	#[test]
	fn recognizes_escaped_segments() {
		for good in ["a", r"a\/b", r"\\", r"\/", r"a\\\/b", "*"] {
			assert!(is_segment(good), "{good:?}");
		}
		for bad in ["a/b", r"a\", r"a\b", r"\*", "/", r"a\\\"] {
			assert!(!is_segment(bad), "{bad:?}");
		}
	}
}
