use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use super::Patterns;

/// Why an exact pattern intersection could not be represented safely.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntersectionError {
	/// The exact intersection would contain too many distinct patterns.
	TooManyPatterns,
}

impl fmt::Display for IntersectionError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::TooManyPatterns => write!(f, "pattern intersection exceeds the complexity limit"),
		}
	}
}

impl std::error::Error for IntersectionError {}

/// Why a string or a segment list is not a valid [`Pattern`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidPattern {
	/// A segment is empty: a leading, trailing, or doubled `/`.
	EmptySegment,
	/// A segment is malformed: a literal, prefix, or suffix contains `/` or `*`, a
	/// partial has neither prefix nor suffix (that is a wildcard), or a segment has
	/// more than one `*`, which is reserved.
	InvalidSegment(String),
	/// More than one `**`.
	MultipleGlobstars,
	/// More than [`Pattern::MAX_SEGMENTS`] segments.
	TooManySegments,
}

impl fmt::Display for InvalidPattern {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::EmptySegment => write!(f, "empty path segment"),
			Self::InvalidSegment(segment) => write!(f, "invalid pattern segment: {segment:?}"),
			Self::MultipleGlobstars => write!(f, "more than one ** segment"),
			Self::TooManySegments => write!(f, "more than {} segments", Pattern::MAX_SEGMENTS),
		}
	}
}

impl std::error::Error for InvalidPattern {}

/// One segment of a [`Pattern`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Segment {
	/// Matches exactly this segment. Never empty, and never contains `/` or `*`.
	Literal(String),
	/// `*`: matches any one segment.
	Wildcard,
	/// `prefix*suffix`: matches any one segment that starts with `prefix` and ends
	/// with `suffix`, without the two overlapping. Either may be empty, not both.
	Partial {
		/// What the segment must start with; may be empty.
		prefix: String,
		/// What the segment must end with; may be empty.
		suffix: String,
	},
	/// `**`: matches any run of zero or more segments. At most one per pattern.
	Globstar,
}

impl Segment {
	/// Parse one segment of a pattern's text.
	fn parse(text: &str) -> Result<Self, InvalidPattern> {
		match text {
			"" => Err(InvalidPattern::EmptySegment),
			"*" => Ok(Self::Wildcard),
			"**" => Ok(Self::Globstar),
			_ if text.contains('/') => Err(InvalidPattern::InvalidSegment(text.to_string())),
			_ => match text.split_once('*') {
				None => Ok(Self::Literal(text.to_string())),
				Some((prefix, suffix)) if !suffix.contains('*') => Ok(Self::Partial {
					prefix: prefix.to_string(),
					suffix: suffix.to_string(),
				}),
				// More than one star in a segment is reserved.
				Some(_) => Err(InvalidPattern::InvalidSegment(text.to_string())),
			},
		}
	}

	/// Whether every segment this one matches, `other` matches too.
	///
	/// `**` is excluded: it spans segments, so containment handles it structurally.
	fn covers(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::Wildcard, Self::Literal(_) | Self::Partial { .. } | Self::Wildcard) => true,
			(Self::Literal(a), Self::Literal(b)) => a == b,
			(Self::Partial { .. }, Self::Literal(literal)) => self.matches(literal),
			// `p*s` covers `p'*s'` exactly when `p` starts `p'` and `s` ends `s'`: the
			// middle is free on both sides, so nothing else can constrain it.
			(
				Self::Partial { prefix, suffix },
				Self::Partial {
					prefix: other_prefix,
					suffix: other_suffix,
				},
			) => other_prefix.starts_with(prefix.as_str()) && other_suffix.ends_with(suffix.as_str()),
			_ => false,
		}
	}

	/// Whether some path segment matches both. Same exclusion as [`covers`](Self::covers).
	fn compatible(&self, other: &Self) -> bool {
		match (self, other) {
			// Two partials meet when one prefix starts the other and one suffix ends
			// the other: the longer prefix followed by the longer suffix matches both.
			(
				Self::Partial { prefix, suffix },
				Self::Partial {
					prefix: other_prefix,
					suffix: other_suffix,
				},
			) => {
				(prefix.starts_with(other_prefix.as_str()) || other_prefix.starts_with(prefix.as_str()))
					&& (suffix.ends_with(other_suffix.as_str()) || other_suffix.ends_with(suffix.as_str()))
			}
			_ => self.covers(other) || other.covers(self),
		}
	}

	/// Whether this segment matches one path segment.
	fn matches(&self, part: &str) -> bool {
		match self {
			Self::Literal(literal) => literal == part,
			Self::Wildcard => true,
			Self::Partial { prefix, suffix } => {
				part.len() >= prefix.len() + suffix.len()
					&& part.starts_with(prefix.as_str())
					&& part.ends_with(suffix.as_str())
			}
			Self::Globstar => false,
		}
	}

	/// The segments matching exactly the parts both match. Same exclusion as
	/// [`covers`](Self::covers). Empty when the two are incompatible.
	fn intersect(&self, other: &Self) -> Vec<Self> {
		match (self, other) {
			(Self::Globstar, _) | (_, Self::Globstar) => Vec::new(),
			(Self::Wildcard, other) => vec![other.clone()],
			(this, Self::Wildcard) => vec![this.clone()],
			(Self::Literal(a), Self::Literal(b)) => (a == b).then(|| self.clone()).into_iter().collect(),
			(Self::Literal(literal), partial @ Self::Partial { .. })
			| (partial @ Self::Partial { .. }, Self::Literal(literal)) => partial
				.matches(literal)
				.then(|| Self::Literal(literal.clone()))
				.into_iter()
				.collect(),
			(
				Self::Partial { prefix, suffix },
				Self::Partial {
					prefix: other_prefix,
					suffix: other_suffix,
				},
			) => {
				if !self.compatible(other) {
					return Vec::new();
				}
				// The longer prefix and the longer suffix pin every part long enough to
				// hold both without overlapping. Shorter parts exist too, where the two
				// runs share bytes: those are finitely many literals.
				let prefix = if prefix.len() >= other_prefix.len() {
					prefix
				} else {
					other_prefix
				};
				let suffix = if suffix.len() >= other_suffix.len() {
					suffix
				} else {
					other_suffix
				};
				let mut out = vec![Self::Partial {
					prefix: prefix.clone(),
					suffix: suffix.clone(),
				}];
				for overlap in 1..=prefix.len().min(suffix.len()) {
					if !prefix.is_char_boundary(prefix.len() - overlap) || !suffix.is_char_boundary(overlap) {
						continue;
					}
					if prefix[prefix.len() - overlap..] != suffix[..overlap] {
						continue;
					}
					let part = format!("{prefix}{}", &suffix[overlap..]);
					if self.matches(&part) && other.matches(&part) {
						out.push(Self::Literal(part));
					}
				}
				out
			}
		}
	}
}

/// Every segment-wise intersection of two runs of the same length: the cartesian
/// product of [`Segment::intersect`] per position. Empty when any position is
/// incompatible.
fn intersect_run(a: &[Segment], b: &[Segment], limit: usize) -> Result<Vec<Vec<Segment>>, IntersectionError> {
	debug_assert_eq!(a.len(), b.len());
	let mut out: Vec<Vec<Segment>> = vec![Vec::with_capacity(a.len())];
	for (a, b) in a.iter().zip(b) {
		let choices = a.intersect(b);
		if choices.is_empty() {
			return Ok(Vec::new());
		}
		if out.len().checked_mul(choices.len()).is_none_or(|size| size > limit) {
			return Err(IntersectionError::TooManyPatterns);
		}
		out = out
			.iter()
			.flat_map(|prefix| {
				choices.iter().map(move |choice| {
					let mut next = prefix.clone();
					next.push(choice.clone());
					next
				})
			})
			.collect();
	}
	Ok(out)
}

fn insert_intersection(
	out: &mut Patterns,
	remaining: &mut usize,
	segments: Vec<Segment>,
) -> Result<(), IntersectionError> {
	if *remaining == 0 {
		return Err(IntersectionError::TooManyPatterns);
	}
	*remaining -= 1;
	if let Ok(pattern) = Pattern::new(segments) {
		out.insert(pattern);
	}
	Ok(())
}

fn intersect_into(
	a: &[Segment],
	b: &[Segment],
	out: &mut Patterns,
	remaining: &mut usize,
) -> Result<(), IntersectionError> {
	for run in intersect_run(a, b, *remaining)? {
		insert_intersection(out, remaining, run)?;
	}
	Ok(())
}

/// `head`, then `**` stretched to `len` segments as `*`, then `tail`.
fn expand(head: &[Segment], tail: &[Segment], len: usize) -> Vec<Segment> {
	let mut out = head.to_vec();
	out.extend(std::iter::repeat_n(Segment::Wildcard, len - head.len() - tail.len()));
	out.extend_from_slice(tail);
	out
}

impl fmt::Display for Segment {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Literal(literal) => f.write_str(literal),
			Self::Wildcard => f.write_str("*"),
			Self::Partial { prefix, suffix } => write!(f, "{prefix}*{suffix}"),
			Self::Globstar => f.write_str("**"),
		}
	}
}

/// How much of a path a pattern pins down, for ranking the patterns that match one path.
///
/// Greater is more specific. The order is total and agrees with containment: when `a`
/// matches a strict superset of `b`'s paths, `a.specificity() < b.specificity()`. Patterns
/// that compare equal without being equal (`*/a` and `*/b`) form one tier; what breaks
/// that tie is the caller's business.
///
/// Compared in order: literal segments (more wins), then no `**` beats `**`, then
/// partial segments (more wins), then `*` segments (more wins), then the bytes the
/// partials pin (more wins), then the length of the literal head.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specificity {
	literals: usize,
	exact: bool,
	partials: usize,
	wildcards: usize,
	pinned: usize,
	head: usize,
}

/// A pattern over broadcast paths: literal segments, `*` for one segment, `prefix*suffix`
/// for one segment with a known start and end, and at most one `**` for any run of
/// segments. Every segment kind matches whole segments, and a pattern is exact: `foo`
/// matches only `foo`, and a subtree is `foo/**`.
///
/// Build one with [`FromStr`] (`"a/*/**".parse()`), [`new`](Self::new) from segments,
/// or [`literal`](Self::literal) and [`subtree`](Self::subtree) from a path. Equality and
/// ordering are by text, which is canonical: two patterns match the same paths when
/// they are equal, and only then. Construction moves `**` before adjacent `*` segments.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Pattern {
	text: String,
	segments: Vec<Segment>,
	// Index of the `**` segment, if any.
	globstar: Option<usize>,
	// Byte length of the literal head within `text`.
	head: usize,
}

impl Pattern {
	/// The most segments a pattern may have, matching the path limit on the wire.
	pub const MAX_SEGMENTS: usize = 32;
	/// The most patterns one exact intersection may produce.
	pub const MAX_INTERSECTIONS: usize = 1024;

	/// A pattern from its segments, validating the grammar and moving `**` before adjacent `*` segments.
	pub fn new(segments: impl IntoIterator<Item = Segment>) -> Result<Self, InvalidPattern> {
		let mut segments: Vec<Segment> = segments.into_iter().collect();
		if segments.len() > Self::MAX_SEGMENTS {
			return Err(InvalidPattern::TooManySegments);
		}

		let mut globstar = None;
		for (i, segment) in segments.iter().enumerate() {
			match segment {
				Segment::Literal(literal) if literal.is_empty() => return Err(InvalidPattern::EmptySegment),
				Segment::Literal(literal) if literal.contains(['*', '/']) => {
					return Err(InvalidPattern::InvalidSegment(literal.clone()));
				}
				Segment::Partial { prefix, suffix }
					if (prefix.is_empty() && suffix.is_empty())
						|| prefix.contains(['*', '/'])
						|| suffix.contains(['*', '/']) =>
				{
					return Err(InvalidPattern::InvalidSegment(format!("{prefix}*{suffix}")));
				}
				Segment::Globstar if globstar.is_some() => return Err(InvalidPattern::MultipleGlobstars),
				Segment::Globstar => globstar = Some(i),
				_ => {}
			}
		}

		// Adjacent `*` and `**` commute; keep `**` first for one language identity.
		if let Some(mut index) = globstar {
			while index > 0 && segments[index - 1] == Segment::Wildcard {
				segments.swap(index - 1, index);
				index -= 1;
			}
			globstar = Some(index);
		}

		let mut text = String::new();
		let mut head = 0;
		let mut in_head = true;
		for (i, segment) in segments.iter().enumerate() {
			if i > 0 {
				text.push('/');
			}
			match segment {
				Segment::Literal(literal) => text.push_str(literal),
				other => {
					in_head = false;
					text.push_str(&other.to_string());
				}
			}
			if in_head {
				head = text.len();
			}
		}

		Ok(Self {
			text,
			segments,
			globstar,
			head,
		})
	}

	/// The pattern matching every path: `**`.
	pub fn all() -> Self {
		Self::new([Segment::Globstar]).expect("** is valid")
	}

	/// The pattern matching exactly `path`.
	///
	/// The path is normalized like a broadcast path (slashes trimmed and collapsed), so
	/// `/foo//bar/` is `foo/bar`. Fails when a segment is `*` or `**`, or contains `*`:
	/// those are wildcards, and a path using them cannot be named by a pattern.
	pub fn literal(path: &str) -> Result<Self, InvalidPattern> {
		Self::new(literal_segments(path))
	}

	/// The pattern matching `path` and every path beneath it: `path/**`.
	///
	/// Normalizes and validates `path` like [`literal`](Self::literal). The empty path
	/// yields `**`.
	pub fn subtree(path: &str) -> Result<Self, InvalidPattern> {
		Self::new(literal_segments(path).chain([Segment::Globstar]))
	}

	/// The canonical text: segments joined by `/`, wildcards as `*` and `**`.
	pub fn as_str(&self) -> &str {
		&self.text
	}

	/// The segments, in order.
	pub fn segments(&self) -> &[Segment] {
		&self.segments
	}

	/// The literal segments before the first wildcard, as a path.
	///
	/// Every matching path starts with it, so it is where a tree walk starts. Empty
	/// when the pattern starts with a wildcard; the whole pattern when it has none.
	pub fn head(&self) -> &str {
		&self.text[..self.head]
	}

	/// Whether the pattern has no wildcards, so it matches exactly one path.
	pub fn is_literal(&self) -> bool {
		self.head == self.text.len()
	}

	/// The covered prefix if this pattern is prefix-shaped: zero or more literals then `**`.
	///
	/// `**` covers every path (the empty prefix). `foo/**` covers `foo` and everything
	/// beneath it. A literal, a `*`, or a `**` that is not last is `None`.
	pub fn as_prefix(&self) -> Option<&str> {
		match self.segments.split_last() {
			Some((Segment::Globstar, head)) if head.iter().all(|s| matches!(s, Segment::Literal(_))) => {
				Some(self.head())
			}
			_ => None,
		}
	}

	/// Whether the pattern has a `**`, so it matches paths of more than one length.
	pub fn has_globstar(&self) -> bool {
		self.globstar.is_some()
	}

	/// Whether `path` is in the set this pattern describes.
	///
	/// The path is normalized like a broadcast path: slashes are trimmed and collapsed.
	pub fn matches(&self, path: &str) -> bool {
		let parts: Vec<&str> = split_path(path).collect();
		match self.globstar {
			None => {
				parts.len() == self.segments.len()
					&& self
						.segments
						.iter()
						.zip(&parts)
						.all(|(segment, part)| segment.matches(part))
			}
			Some(_) => {
				let (head, tail) = self.split();
				parts.len() >= head.len() + tail.len()
					&& head.iter().zip(&parts).all(|(segment, part)| segment.matches(part))
					&& tail
						.iter()
						.rev()
						.zip(parts.iter().rev())
						.all(|(segment, part)| segment.matches(part))
			}
		}
	}

	/// Whether every path `other` matches, this pattern matches too.
	///
	/// This is the authorization check: a grant contains a request when the request
	/// cannot name a path outside it. A pattern contains itself.
	pub fn contains(&self, other: &Self) -> bool {
		match (self.globstar, other.globstar) {
			(None, None) => {
				self.segments.len() == other.segments.len()
					&& self.segments.iter().zip(&other.segments).all(|(a, b)| a.covers(b))
			}
			// A fixed-length pattern cannot contain one that matches many lengths.
			(None, Some(_)) => false,
			(Some(_), None) => {
				let (head, tail) = self.split();
				other.segments.len() >= head.len() + tail.len()
					&& head.iter().zip(&other.segments).all(|(a, b)| a.covers(b))
					&& tail
						.iter()
						.rev()
						.zip(other.segments.iter().rev())
						.all(|(a, b)| a.covers(b))
			}
			(Some(_), Some(_)) => {
				let (head, tail) = self.split();
				let (other_head, other_tail) = other.split();

				// The other's `**` can be arbitrarily long, so any of our segments that
				// reach past the other's head or tail must be `*`; and the other's
				// shortest path (its `**` empty) must still be long enough for ours.
				let covers_run = |ours: &[Segment], theirs: &[Segment]| {
					ours.iter().enumerate().all(|(i, a)| match theirs.get(i) {
						Some(b) => a.covers(b),
						None => *a == Segment::Wildcard,
					})
				};
				let reversed = |run: &[Segment]| run.iter().rev().cloned().collect::<Vec<_>>();

				head.len() + tail.len() <= other_head.len() + other_tail.len()
					&& covers_run(head, other_head)
					&& covers_run(&reversed(tail), &reversed(other_tail))
			}
		}
	}

	/// Whether some path matches both patterns.
	pub fn overlaps(&self, other: &Self) -> bool {
		let compatible_run = |a: &[Segment], b: &[Segment]| a.iter().zip(b).all(|(a, b)| a.compatible(b));
		let compatible_tail =
			|a: &[Segment], b: &[Segment]| a.iter().rev().zip(b.iter().rev()).all(|(a, b)| a.compatible(b));

		match (self.globstar, other.globstar) {
			(None, None) => {
				self.segments.len() == other.segments.len() && compatible_run(&self.segments, &other.segments)
			}
			(None, Some(_)) => other.overlaps(self),
			(Some(_), None) => {
				let (head, tail) = self.split();
				other.segments.len() >= head.len() + tail.len()
					&& compatible_run(head, &other.segments)
					&& compatible_tail(tail, &other.segments)
			}
			(Some(_), Some(_)) => {
				// A path long enough keeps the heads and tails apart, so the only
				// constraints are segment-wise where the heads and tails overlap.
				let (head, tail) = self.split();
				let (other_head, other_tail) = other.split();
				compatible_run(head, other_head) && compatible_tail(tail, other_tail)
			}
		}
	}

	/// How much of a path this pattern pins down. See [`Specificity`].
	pub fn specificity(&self) -> Specificity {
		let count = |wanted: fn(&Segment) -> bool| self.segments.iter().filter(|s| wanted(s)).count();
		Specificity {
			literals: count(|s| matches!(s, Segment::Literal(_))),
			exact: self.globstar.is_none(),
			partials: count(|s| matches!(s, Segment::Partial { .. })),
			wildcards: count(|s| matches!(s, Segment::Wildcard)),
			pinned: self
				.segments
				.iter()
				.map(|s| match s {
					Segment::Partial { prefix, suffix } => prefix.len() + suffix.len(),
					_ => 0,
				})
				.sum(),
			head: self
				.segments
				.iter()
				.take_while(|s| matches!(s, Segment::Literal(_)))
				.count(),
		}
	}

	/// The patterns that, relative to `root`, match exactly the paths this pattern
	/// matches beneath `root`.
	///
	/// This is how a grant or an advertisement is presented inside a rooted view. It is
	/// a set because `**` may consume the root or stop short of it: `**/a` rebased at `a`
	/// is both the empty pattern (the root itself) and `**/a` (deeper paths ending in
	/// `a`). Empty when nothing under `root` matches. The root is normalized like a
	/// broadcast path.
	pub fn rebase(&self, root: &str) -> Patterns {
		let root: Vec<&str> = split_path(root).collect();
		let mut out = Patterns::new();

		let matches_run = |segments: &[Segment], parts: &[&str]| segments.iter().zip(parts).all(|(s, p)| s.matches(p));
		// Construction cannot fail: the segments come from a valid pattern, and a
		// rebase never lengthens it.
		let build = |segments: &[Segment]| Pattern::new(segments.to_vec()).expect("a rebased pattern is valid");

		match self.globstar {
			None => {
				if root.len() <= self.segments.len() && matches_run(&self.segments, &root) {
					out.insert(build(&self.segments[root.len()..]));
				}
			}
			Some(index) => {
				let (head, tail) = self.split();
				if root.len() <= head.len() {
					if matches_run(head, &root) {
						out.insert(build(&self.segments[root.len()..]));
					}
					return out;
				}
				if !matches_run(head, &root) {
					return out;
				}

				// The root reaches into the `**`. Either the `**` swallows the rest of the
				// root and stays open, or it closed inside the root and some of the tail
				// already matched the root's last segments.
				let rest = &root[head.len()..];
				out.insert(build(&self.segments[index..]));
				for consumed in 1..=tail.len().min(rest.len()) {
					if matches_run(&tail[..consumed], &rest[rest.len() - consumed..]) {
						out.insert(build(&tail[consumed..]));
					}
				}
			}
		}

		out
	}

	/// The patterns matching exactly the paths both patterns match.
	///
	/// This is how a claim is clamped to a scope: the covered paths inside the
	/// grant, as patterns of their own. It is a set because two partial segments or
	/// two `**` runs can meet in more than one way: `ab*` and `*b` meet at `ab*b` and
	/// at `ab`, and `a/**` and `**/a` meet at `a/**/a` and at `a`. Empty when the
	/// two do not [overlap](Self::overlaps).
	pub fn intersect(&self, other: &Self) -> Result<Patterns, IntersectionError> {
		// The contained pattern is the intersection as written, where the general
		// case below could only spell the same set in more pieces.
		if self.contains(other) {
			return Ok(Patterns::from(other.clone()));
		}
		if other.contains(self) {
			return Ok(Patterns::from(self.clone()));
		}

		let mut out = Patterns::new();
		let mut remaining = Self::MAX_INTERSECTIONS;

		match (self.globstar, other.globstar) {
			(None, None) => {
				if self.segments.len() == other.segments.len() {
					intersect_into(&self.segments, &other.segments, &mut out, &mut remaining)?;
				}
			}
			(Some(_), None) => {
				let (head, tail) = self.split();
				if other.segments.len() >= head.len() + tail.len() {
					let stretched = expand(head, tail, other.segments.len());
					intersect_into(&stretched, &other.segments, &mut out, &mut remaining)?;
				}
			}
			(None, Some(_)) => return other.intersect(self),
			(Some(_), Some(_)) => {
				let (head, tail) = self.split();
				let (other_head, other_tail) = other.split();
				let heads = head.len().max(other_head.len());
				let tails = tail.len().max(other_tail.len());
				let shortest = (head.len() + tail.len()).max(other_head.len() + other_tail.len());

				// Paths too short to keep the longer head and the longer tail apart
				// constrain both from each end at once: enumerate each length. When
				// the open form below would not fit, every length that fits is short.
				let long = heads + tails;
				let open = long < Self::MAX_SEGMENTS;
				let cap = if open { long } else { Self::MAX_SEGMENTS + 1 };
				for len in shortest..cap {
					let a = expand(head, tail, len);
					let b = expand(other_head, other_tail, len);
					intersect_into(&a, &b, &mut out, &mut remaining)?;
				}

				// Longer paths pin the heads and the tails independently and leave the
				// run between them free.
				if open {
					let pad = |run: &[Segment], len: usize, front: bool| -> Vec<Segment> {
						let fill = std::iter::repeat_n(Segment::Wildcard, len - run.len());
						if front {
							run.iter().cloned().chain(fill).collect()
						} else {
							fill.chain(run.iter().cloned()).collect()
						}
					};
					let fronts = intersect_run(&pad(head, heads, true), &pad(other_head, heads, true), remaining)?;
					let backs = intersect_run(&pad(tail, tails, false), &pad(other_tail, tails, false), remaining)?;
					if fronts
						.len()
						.checked_mul(backs.len())
						.is_none_or(|size| size > remaining)
					{
						return Err(IntersectionError::TooManyPatterns);
					}
					for front in &fronts {
						for back in &backs {
							let mut segments = front.clone();
							segments.push(Segment::Globstar);
							segments.extend_from_slice(back);
							insert_intersection(&mut out, &mut remaining, segments)?;
						}
					}
				}
			}
		}

		Ok(out)
	}

	/// What each wildcard of this pattern stands for in `matched`, a pattern this
	/// one [contains](Self::contains); `None` when it does not.
	///
	/// One capture per non-literal segment (`*`, `prefix*suffix`, `**`), in order,
	/// the way a regex match exposes its groups: `foo/*/chat` against `foo/alice/chat`
	/// captures `alice`, and `foo/**` against `foo/alice/chat` captures `alice/chat`.
	/// A capture is a pattern because `matched` may be one: `foo/**` against
	/// `foo/alice/**` captures `alice/**`. When `matched` has a `**` that this
	/// pattern's own segments straddle (`**/*` against `a/**`, where the last
	/// segment is `a` or anything after it), the segments it straddles cannot be
	/// pinned and capture themselves: `**` then `*`.
	pub fn captures(&self, matched: &Self) -> Option<Vec<Self>> {
		if !self.contains(matched) {
			return None;
		}
		// Construction cannot fail: every capture is a run of `matched`'s own valid
		// segments or one of ours, never longer than either.
		let build = |segments: &[Segment]| Pattern::new(segments.to_vec()).expect("a capture is valid");
		let mut out = Vec::new();

		if self.globstar.is_none() {
			for (segment, theirs) in self.segments.iter().zip(&matched.segments) {
				if !matches!(segment, Segment::Literal(_)) {
					out.push(build(std::slice::from_ref(theirs)));
				}
			}
			return Some(out);
		}

		let (head, tail) = self.split();
		let middle = matched.segments.len() - tail.len();
		// Our head aligns with `matched` from the front and our tail from the back.
		// A segment of ours aligned at or beyond `matched`'s `**` (from its own
		// side) has no fixed counterpart, so it captures itself.
		let free = matched.globstar;
		let pinned = |at: usize, from_front: bool| match free {
			Some(free) if from_front => at < free,
			Some(free) => at > free,
			None => true,
		};

		for (i, segment) in head.iter().enumerate() {
			if !matches!(segment, Segment::Literal(_)) {
				let capture = if pinned(i, true) { &matched.segments[i] } else { segment };
				out.push(build(std::slice::from_ref(capture)));
			}
		}
		if free.is_none_or(|free| free >= head.len() && free < middle) {
			out.push(build(&matched.segments[head.len()..middle]));
		} else {
			out.push(Pattern::all());
		}
		for (j, segment) in tail.iter().enumerate() {
			if !matches!(segment, Segment::Literal(_)) {
				let at = middle + j;
				let capture = if pinned(at, false) {
					&matched.segments[at]
				} else {
					segment
				};
				out.push(build(std::slice::from_ref(capture)));
			}
		}
		Some(out)
	}

	/// This pattern placed beneath a literal `root`: the same paths, named from the
	/// root's parent. The inverse of [`rebase`](Self::rebase) for a single pattern.
	///
	/// The root is normalized and validated like [`literal`](Self::literal), and the
	/// result must fit [`MAX_SEGMENTS`](Self::MAX_SEGMENTS).
	pub fn rooted(&self, root: &str) -> Result<Self, InvalidPattern> {
		Self::new(literal_segments(root).chain(self.segments.iter().cloned()))
	}

	/// The segments before and after the `**`. Only meaningful when there is one.
	fn split(&self) -> (&[Segment], &[Segment]) {
		match self.globstar {
			Some(index) => (&self.segments[..index], &self.segments[index + 1..]),
			None => (&self.segments, &[]),
		}
	}
}

/// The non-empty segments of a path: leading, trailing, and doubled slashes are dropped,
/// matching how a broadcast path is normalized.
fn split_path(path: &str) -> impl Iterator<Item = &str> {
	path.split('/').filter(|part| !part.is_empty())
}

/// The segments of a path as literals, leaving validation to [`Pattern::new`].
fn literal_segments(path: &str) -> impl Iterator<Item = Segment> + '_ {
	// A `*` or `**` segment becomes an invalid literal, which `new` rejects: a path
	// is never read as a pattern.
	split_path(path).map(|part| Segment::Literal(part.to_string()))
}

impl FromStr for Pattern {
	type Err = InvalidPattern;

	/// Parse a pattern's text. Unlike a path, slashes are not normalized: a leading,
	/// trailing, or doubled `/` is an error, so a typo cannot silently widen a grant.
	fn from_str(text: &str) -> Result<Self, InvalidPattern> {
		if text.is_empty() {
			return Self::new([]);
		}
		text.split('/')
			.map(Segment::parse)
			.collect::<Result<Vec<_>, _>>()
			.and_then(Self::new)
	}
}

impl TryFrom<&str> for Pattern {
	type Error = InvalidPattern;

	fn try_from(text: &str) -> Result<Self, InvalidPattern> {
		text.parse()
	}
}

impl TryFrom<String> for Pattern {
	type Error = InvalidPattern;

	fn try_from(text: String) -> Result<Self, InvalidPattern> {
		text.parse()
	}
}

impl Default for Pattern {
	/// The empty pattern, which matches only the empty path.
	fn default() -> Self {
		Self::new([]).expect("the empty pattern is valid")
	}
}

impl fmt::Display for Pattern {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.text)
	}
}

impl fmt::Debug for Pattern {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Pattern({:?})", self.text)
	}
}

impl PartialOrd for Pattern {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Pattern {
	/// Ordered by text, so a sorted list of patterns is deterministic.
	fn cmp(&self, other: &Self) -> Ordering {
		self.text.cmp(&other.text)
	}
}

#[cfg(feature = "serde")]
impl serde::Serialize for Pattern {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Pattern {
	/// Reads the canonical text, so a persisted pattern is validated on the way in.
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

impl AsRef<str> for Pattern {
	fn as_ref(&self) -> &str {
		&self.text
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pattern(text: &str) -> Pattern {
		text.parse().unwrap_or_else(|err| panic!("{text:?}: {err}"))
	}

	#[test]
	fn parses_and_prints_canonically() {
		for text in [
			"",
			"a",
			"a/b",
			"*",
			"**",
			"a/*/b",
			"**/transcode.pro",
			"a/**/b/*",
			"**/*",
			"**/*.hang",
			"foo*",
			"foo.*.hang",
		] {
			assert_eq!(pattern(text).to_string(), text);
		}
		assert_eq!(
			pattern("a/*/**/b").segments(),
			&[
				Segment::Literal("a".into()),
				Segment::Globstar,
				Segment::Wildcard,
				Segment::Literal("b".into()),
			]
		);
	}

	#[test]
	fn rejects_bad_syntax() {
		assert_eq!("/a".parse::<Pattern>(), Err(InvalidPattern::EmptySegment));
		assert_eq!("a/".parse::<Pattern>(), Err(InvalidPattern::EmptySegment));
		assert_eq!("a//b".parse::<Pattern>(), Err(InvalidPattern::EmptySegment));
		assert_eq!("/".parse::<Pattern>(), Err(InvalidPattern::EmptySegment));
		assert_eq!("**/**".parse::<Pattern>(), Err(InvalidPattern::MultipleGlobstars));
		assert_eq!(
			"***".parse::<Pattern>(),
			Err(InvalidPattern::InvalidSegment("***".into()))
		);

		assert_eq!(
			"a*b*c".parse::<Pattern>(),
			Err(InvalidPattern::InvalidSegment("a*b*c".into()))
		);
		assert_eq!(
			"*a*".parse::<Pattern>(),
			Err(InvalidPattern::InvalidSegment("*a*".into()))
		);
		assert_eq!(
			"*.hang".parse::<Pattern>().unwrap().segments(),
			&[Segment::Partial {
				prefix: String::new(),
				suffix: ".hang".into()
			}]
		);
		assert_eq!(
			Pattern::new([Segment::Partial {
				prefix: String::new(),
				suffix: String::new()
			}]),
			Err(InvalidPattern::InvalidSegment("*".into()))
		);
		assert_eq!(
			Pattern::new([Segment::Partial {
				prefix: "a/".into(),
				suffix: String::new()
			}]),
			Err(InvalidPattern::InvalidSegment("a/*".into()))
		);

		let deep = ["a"; Pattern::MAX_SEGMENTS + 1].join("/");
		assert_eq!(deep.parse::<Pattern>(), Err(InvalidPattern::TooManySegments));
		let max = ["a"; Pattern::MAX_SEGMENTS].join("/");
		assert!(max.parse::<Pattern>().is_ok());

		assert_eq!(
			Pattern::new([Segment::Literal("a/b".into())]),
			Err(InvalidPattern::InvalidSegment("a/b".into()))
		);
		assert_eq!(
			Pattern::new([Segment::Literal(String::new())]),
			Err(InvalidPattern::EmptySegment)
		);
	}

	#[test]
	fn literal_and_subtree_normalize_paths() {
		assert_eq!(Pattern::literal("/foo//bar/").unwrap(), pattern("foo/bar"));
		assert_eq!(Pattern::literal("").unwrap(), Pattern::default());
		assert_eq!(Pattern::subtree("foo").unwrap(), pattern("foo/**"));
		assert_eq!(Pattern::subtree("/").unwrap(), Pattern::all());
		assert_eq!(Pattern::literal("a/*"), Err(InvalidPattern::InvalidSegment("*".into())));
		assert_eq!(Pattern::literal("**"), Err(InvalidPattern::InvalidSegment("**".into())));
	}

	#[test]
	fn as_prefix_accepts_literals_then_globstar() {
		assert_eq!(Pattern::all().as_prefix(), Some(""));
		assert_eq!(pattern("foo/**").as_prefix(), Some("foo"));
		assert_eq!(pattern("foo/bar/**").as_prefix(), Some("foo/bar"));
		assert_eq!(pattern("foo").as_prefix(), None);
		assert_eq!(Pattern::default().as_prefix(), None);
		assert_eq!(pattern("foo/*").as_prefix(), None);
		assert_eq!(pattern("*/foo/**").as_prefix(), None);
		assert_eq!(pattern("foo/**/bar").as_prefix(), None);
		assert_eq!(pattern("foo*/**").as_prefix(), None);
	}

	#[test]
	fn matches_whole_segments() {
		let cases = [
			("", "", true),
			("", "a", false),
			("a", "a", true),
			("a", "a/b", false),
			("a", "ab", false),
			("*", "a", true),
			("*", "", false),
			("*", "a/b", false),
			("**", "", true),
			("**", "a/b/c", true),
			("a/**", "a", true),
			("a/**", "a/b/c", true),
			("a/**", "b", false),
			("**/c", "c", true),
			("**/c", "a/b/c", true),
			("**/c", "a/c/b", false),
			("a/**/c", "a/c", true),
			("a/**/c", "a/x/y/c", true),
			("a/**/c", "a", false),
			("a/*/c", "a/x/c", true),
			("a/*/c", "a/c", false),
			("a/*/**", "a", false),
			("a/*/**", "a/b", true),
			("**/transcode.pro", "pid/foo.hang/transcode.pro", true),
			("**/transcode.pro", "pid/foo.transcode.pro", false),
			("**/*.hang", "pid/cam.hang", true),
			("**/*.hang", ".hang", true),
			("**/*.hang", "pid/cam.hang/x", false),
			("foo*", "foo", true),
			("foo*", "foobar", true),
			("foo*", "fo", false),
			("foo.*.hang", "foo..hang", true),
			("foo.*.hang", "foo.1.hang", true),
			("foo.*.hang", "foo.hang", false),
			("a*a", "a", false),
			("a*a", "aa", true),
		];
		for (text, path, expected) in cases {
			assert_eq!(pattern(text).matches(path), expected, "{text} vs {path}");
		}
		// Paths normalize like broadcast paths.
		assert!(pattern("a/b").matches("/a//b/"));
	}

	#[test]
	fn contains_is_containment() {
		let cases = [
			("**", "**", true),
			("**", "", true),
			("**", "a/*/b", true),
			("", "**", false),
			("*", "a", true),
			("a", "*", false),
			("a/**", "a", true),
			("a/**", "a/b/**", true),
			("a/**", "**", false),
			("a/**", "**/a", false),
			("**/a", "a", true),
			("**/a", "**/b/a", true),
			("**/a", "a/**", false),
			("*/**", "**", false),
			("*/**", "a/**", true),
			("*/*/**", "a/**", false),
			("*/*/**", "a/b/**", true),
			("a/**/c", "a/c", true),
			("a/**/c", "a/x/c", true),
			("a/**/c", "a/**/x/c", true),
			("a/*/**/*", "a/**/b", false),
			("a/*/c", "a/b/c", true),
			("a/*/c", "a/**/c", false),
			("*", "*.hang", true),
			("*.hang", "*", false),
			("*.hang", "cam.hang", true),
			("*.hang", "cam.hang2", false),
			("*.hang", "*.hang", true),
			("*.hang", "cam*.hang", true),
			("*.hang", "cam*hang", false),
			("foo*", "foo.*.hang", true),
			("foo.*", "foo*", false),
			("**/*.hang", "pid/*/cam.hang", true),
		];
		for (outer, inner, expected) in cases {
			assert_eq!(
				pattern(outer).contains(&pattern(inner)),
				expected,
				"{outer} contains {inner}"
			);
		}
	}

	#[test]
	fn overlaps_is_symmetric_intersection() {
		let cases = [
			("a", "a", true),
			("a", "b", false),
			("a", "*", true),
			("a", "a/*", false),
			("a/**", "**/b", true),
			("a/**", "b/**", false),
			("a/*", "*/b", true),
			("a/*", "b/*", false),
			("*/*", "a/**", true),
			("*", "a/**", true),
			("*", "a/*/**", false),
			("**", "", true),
			("a/**/b", "**/c", false),
			("a/**/b", "**/*", true),
			("*.hang", "cam*", true),
			("*.hang", "cam.msf", false),
			("*.hang", "*.msf", false),
			("foo*", "foo.bar*", true),
			("foo*", "fob*", false),
			("a*b", "ab", true),
			("ab*", "*ab", true),
			("a/**/b", "x/**", false),
			("a/**/b", "**/x", false),
		];
		for (a, b, expected) in cases {
			assert_eq!(pattern(a).overlaps(&pattern(b)), expected, "{a} overlaps {b}");
			assert_eq!(pattern(b).overlaps(&pattern(a)), expected, "{b} overlaps {a}");
		}
	}

	#[test]
	fn specificity_ranks_by_what_is_pinned_down() {
		// Strictly descending.
		let ranked = ["a/b/c", "a/b", "a/*.hang", "a/*", "a/**", "*.hang", "*", "**"];
		for pair in ranked.windows(2) {
			assert!(
				pattern(pair[0]).specificity() > pattern(pair[1]).specificity(),
				"{} should outrank {}",
				pair[0],
				pair[1]
			);
		}
		// A longer literal head breaks otherwise equal ties.
		assert!(pattern("a/**").specificity() > pattern("**/a").specificity());
		// A partial pinning more bytes is more specific than one pinning fewer.
		assert!(pattern("cam*.hang").specificity() > pattern("*.hang").specificity());
		assert!(pattern("*.hang").specificity() > pattern("*").specificity());
		// Equal structure is one tier.
		assert_eq!(pattern("*/a").specificity(), pattern("*/b").specificity());
		assert_eq!(pattern("*/a/**").specificity(), pattern("*/**/a").specificity());
	}

	#[test]
	fn rebase_is_set_valued() {
		let cases: &[(&str, &str, &[&str])] = &[
			("**", "a", &["**"]),
			("**/a", "a", &["", "**/a"]),
			("a/**", "a", &["**"]),
			("a/**", "a/b", &["**"]),
			("a/**", "b", &[]),
			("a/b", "a", &["b"]),
			("a/b", "a/b", &[""]),
			("a/b", "a/b/c", &[]),
			("*/b", "a", &["b"]),
			("a/*/c", "a/x", &["c"]),
			("a/**/b/c", "a/b", &["**/b/c", "c"]),
			("a/**/b", "a/b/b", &["**/b", ""]),
			("**/b/c", "b", &["**/b/c", "c"]),
			("", "", &[""]),
			("", "a", &[]),
			("**", "", &["**"]),
			("*.hang/**", "cam.hang", &["**"]),
			("*.hang/**", "cam.msf", &[]),
			("**/*.hang", "a.hang", &["", "**/*.hang"]),
		];
		for (text, root, expected) in cases {
			let got = pattern(text).rebase(root);
			let expected: Patterns = expected.iter().map(|e| pattern(e)).collect();
			assert_eq!(got, expected, "{text} rebased at {root}");
		}
	}

	#[test]
	fn rooted_inverts_rebase() {
		assert_eq!(pattern("**").rooted("a/b").unwrap(), pattern("a/b/**"));
		assert_eq!(pattern("").rooted("a").unwrap(), pattern("a"));
		assert_eq!(pattern("*/c").rooted("").unwrap(), pattern("*/c"));
		assert_eq!(
			pattern("a").rooted("*"),
			Err(InvalidPattern::InvalidSegment("*".into()))
		);

		let deep = ["a"; Pattern::MAX_SEGMENTS].join("/");
		assert_eq!(pattern("b").rooted(&deep), Err(InvalidPattern::TooManySegments));
	}

	#[test]
	fn head_is_the_literal_prefix() {
		assert_eq!(pattern("a/b/*/c").head(), "a/b");
		assert_eq!(pattern("**/a").head(), "");
		assert_eq!(pattern("a/b").head(), "a/b");
		assert_eq!(pattern("").head(), "");
		assert_eq!(pattern("a/b*/c").head(), "a");
		assert!(!pattern("a/b*").is_literal());
		assert!(pattern("a/b").is_literal());
		assert!(!pattern("a/*").is_literal());
		assert!(pattern("a/**").has_globstar());
		assert!(!pattern("a/*").has_globstar());
	}

	#[cfg(feature = "serde")]
	#[test]
	fn serde_round_trips_as_text() {
		let p = pattern("a/*/**");
		let json = serde_json::to_string(&p).unwrap();
		assert_eq!(json, "\"a/**/*\"");
		assert_eq!(serde_json::from_str::<Pattern>(&json).unwrap(), p);
		assert!(serde_json::from_str::<Pattern>("\"a//b\"").is_err());
	}

	#[test]
	fn ordering_is_by_text() {
		let mut list = [pattern("b"), pattern("**"), pattern("a/*"), pattern("a")];
		list.sort();
		let texts: Vec<_> = list.iter().map(ToString::to_string).collect();
		assert_eq!(texts, ["**", "a", "a/*", "b"]);
	}
}
