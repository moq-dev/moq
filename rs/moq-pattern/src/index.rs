//! A transient index over a union's members, so a batch of containment or overlap
//! queries costs the trie paths each query can follow instead of a scan of every member.

use super::{Pattern, Segment};

/// A segment trie over borrowed patterns.
///
/// A fixed-length member ends at the node its segments lead to. A member with `**` hangs
/// its tail, reversed, off the node its head leads to, so a query reaches it from both
/// ends. Built for one batch and dropped after: holding one would multiply a union's
/// memory.
#[derive(Default)]
pub(crate) struct Index<'a> {
	root: Node<'a>,
}

impl<'a> Index<'a> {
	/// An index over `members`.
	pub(crate) fn new(members: impl IntoIterator<Item = &'a Pattern>) -> Self {
		let mut index = Self::default();
		for member in members {
			index.insert(member);
		}
		index
	}

	/// Index one more member.
	pub(crate) fn insert(&mut self, member: &'a Pattern) {
		let (head, tail) = member.split();
		let mut node = &mut self.root;
		for segment in head {
			node = node.child(segment);
		}
		if member.has_globstar() {
			node = node.tails.get_or_insert_default();
			for segment in tail.iter().rev() {
				node = node.child(segment);
			}
		}
		node.end = Some(member);
	}

	/// Whether some member contains `query`.
	pub(crate) fn contains(&self, query: &Pattern) -> bool {
		let mut found = false;
		self.walk(query, Relation::Contains, &mut |member| {
			debug_assert!(member.contains(query), "{member} does not contain {query}");
			found = true;
			true
		});
		found
	}

	/// The members that overlap `query`.
	pub(crate) fn overlapping(&self, query: &Pattern) -> Vec<&'a Pattern> {
		let mut out = Vec::new();
		self.walk(query, Relation::Overlaps, &mut |member| {
			// The walk pins only what the trie can see from each end; the rest is exact here.
			if member.overlaps(query) {
				out.push(member);
			}
			false
		});
		out
	}

	/// Visit the members `relation` may hold for, until `visit` returns true.
	///
	/// For [`Relation::Contains`] every visited member contains `query`. For
	/// [`Relation::Overlaps`] every overlapping member is visited, among others.
	fn walk(&self, query: &Pattern, relation: Relation, visit: &mut impl FnMut(&'a Pattern) -> bool) {
		let query = Query::new(query);
		// A member that contains an open query has no more fixed segments than it.
		let limit = (relation == Relation::Contains && query.open).then(|| query.head.len() + query.tail.len());

		// Each frame is a node, its depth in its trie, and, inside a tail trie, the length
		// of the head that led there.
		let mut stack: Vec<(&Node<'a>, usize, Option<usize>)> = vec![(&self.root, 0, None)];
		while let Some((node, depth, head)) = stack.pop() {
			let length = head.unwrap_or(0) + depth;
			if let Some(member) = node.end {
				let candidate = match (head, relation) {
					(Some(_), _) => true,
					// A fixed-length member contains only a fixed query of its own length.
					(None, Relation::Contains) => !query.open && depth == query.head.len(),
					(None, Relation::Overlaps) => query.open || depth == query.head.len(),
				};
				if candidate && visit(member) {
					return;
				}
			}
			if head.is_none()
				&& let Some(tails) = &node.tails
			{
				stack.push((tails, 0, Some(depth)));
			}
			if limit.is_some_and(|limit| length >= limit) {
				continue;
			}
			let slot = match head {
				None => query.front(depth),
				Some(head) => query.back(head, depth),
			};
			if let Some(slot) = slot {
				node.children(slot, relation, &mut |child| stack.push((child, depth + 1, head)));
			}
		}
	}
}

/// Which relation a walk tests members against the query with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
	/// The member contains the query.
	Contains,
	/// The member and the query share a path.
	Overlaps,
}

/// What the query pins at one position of a walk.
#[derive(Clone, Copy)]
enum Slot<'q> {
	/// One of the query's segments, never `**`.
	Pinned(&'q Segment),
	/// A position inside the query's `**`, which any one segment can take.
	Free,
}

/// A query seen from each end.
struct Query<'q> {
	/// The segments before the `**`, or all of them.
	head: &'q [Segment],
	/// The segments after the `**`, or all of them.
	tail: &'q [Segment],
	/// Whether the query has a `**`.
	open: bool,
}

impl<'q> Query<'q> {
	fn new(pattern: &'q Pattern) -> Self {
		let (head, tail) = pattern.split();
		match pattern.has_globstar() {
			true => Self { head, tail, open: true },
			false => Self {
				head,
				tail: head,
				open: false,
			},
		}
	}

	/// The query at `depth` from the front, or `None` past a fixed query's end.
	fn front(&self, depth: usize) -> Option<Slot<'q>> {
		match self.head.get(depth) {
			Some(segment) => Some(Slot::Pinned(segment)),
			None => self.open.then_some(Slot::Free),
		}
	}

	/// The query at `depth` from the back, beneath a member head of length `head`, or
	/// `None` where a fixed query has no segment the head has not already taken.
	fn back(&self, head: usize, depth: usize) -> Option<Slot<'q>> {
		let room = match self.open {
			true => self.tail.len(),
			false => self.tail.len().checked_sub(head)?,
		};
		match depth < room {
			true => Some(Slot::Pinned(&self.tail[self.tail.len() - 1 - depth])),
			false => self.open.then_some(Slot::Free),
		}
	}
}

#[derive(Default)]
struct Node<'a> {
	/// Literal children by their bytes, so a partial query finds the literals it could
	/// match without scanning them all.
	literals: Trie<Literal<'a>>,
	wildcard: Option<Box<Node<'a>>>,
	/// Partial children by prefix, then by suffix read backwards: a query walks only the
	/// prefixes and suffixes that start or end its own.
	partials: Trie<Trie<Node<'a>>>,
	/// The member ending here: a fixed-length member in the head trie, or a `**` member
	/// whose reversed tail ends here in a tail trie.
	end: Option<&'a Pattern>,
	/// The reversed tails of the `**` members whose head ends here.
	tails: Option<Box<Node<'a>>>,
}

struct Literal<'a> {
	text: &'a str,
	node: Node<'a>,
}

impl<'a> Node<'a> {
	fn child(&mut self, segment: &'a Segment) -> &mut Node<'a> {
		match segment {
			Segment::Literal(text) => {
				let literal = self.literals.entry(text.bytes(), || Literal {
					text,
					node: Node::default(),
				});
				&mut literal.node
			}
			Segment::Wildcard => self.wildcard.get_or_insert_default(),
			Segment::Partial { prefix, suffix } => self
				.partials
				.entry(prefix.bytes(), Trie::default)
				.entry(suffix.bytes().rev(), Node::default),
			Segment::Globstar => unreachable!("an index splits ** out before walking segments"),
		}
	}

	/// Visit each child whose segment `relation` can hold for against `slot`.
	fn children<'n>(&'n self, slot: Slot<'_>, relation: Relation, visit: &mut impl FnMut(&'n Node<'a>)) {
		if let Some(wildcard) = &self.wildcard {
			// `*` covers, and so overlaps, any one segment.
			visit(wildcard);
		}
		match (slot, relation) {
			// Only `*` contains a position the query leaves free.
			(Slot::Free | Slot::Pinned(Segment::Wildcard), Relation::Contains) => {}
			(Slot::Free | Slot::Pinned(Segment::Wildcard), Relation::Overlaps) => {
				self.literals.below([], &mut |literal| visit(&literal.node));
				self.partials.below([], &mut |suffixes| suffixes.below([], visit));
			}
			(Slot::Pinned(Segment::Literal(text)), _) => {
				if let Some(literal) = self.literals.get(text.bytes()) {
					visit(&literal.node);
				}
				// A partial overlaps a literal exactly when it covers it: its prefix starts the
				// literal and its suffix ends it, without the two overlapping.
				self.partials.above(text.bytes(), text.len(), &mut |prefix, suffixes| {
					suffixes.above(text.bytes().rev(), text.len() - prefix, &mut |_, node| visit(node));
				});
			}
			(Slot::Pinned(Segment::Partial { prefix, suffix }), Relation::Contains) => {
				self.partials.above(prefix.bytes(), usize::MAX, &mut |_, suffixes| {
					suffixes.above(suffix.bytes().rev(), usize::MAX, &mut |_, node| visit(node));
				});
			}
			(Slot::Pinned(partial @ Segment::Partial { prefix, suffix }), Relation::Overlaps) => {
				self.literals.below(prefix.bytes(), &mut |literal| {
					if partial.matches(literal.text) {
						visit(&literal.node);
					}
				});
				// Two partials meet when one prefix starts the other and one suffix ends the other.
				self.partials.related(prefix.bytes(), &mut |suffixes| {
					suffixes.related(suffix.bytes().rev(), visit);
				});
			}
			(Slot::Pinned(Segment::Globstar), _) => unreachable!("a query splits ** out before walking segments"),
		}
	}
}

/// Values keyed by bytes, one level per byte, so a walk along a key meets every value
/// whose key starts it, and the level a key ends at holds every value whose key extends it.
///
/// The levels live in one flat list linked by position, never nested: a key may be as long
/// as a whole message, and a chain of owned levels that long would overflow the stack when
/// dropped.
struct Trie<T> {
	/// The root first; empty until the first entry.
	levels: Vec<Level<T>>,
}

struct Level<T> {
	value: Option<Box<T>>,
	/// Each child's byte and position, sorted by byte.
	children: Vec<(u8, usize)>,
}

impl<T> Default for Trie<T> {
	fn default() -> Self {
		Self { levels: Vec::new() }
	}
}

impl<T> Trie<T> {
	const ROOT: usize = 0;

	fn entry(&mut self, key: impl IntoIterator<Item = u8>, init: impl FnOnce() -> T) -> &mut T {
		if self.levels.is_empty() {
			self.levels.push(Level::new());
		}
		let mut at = Self::ROOT;
		for byte in key {
			at = match self.levels[at].children.binary_search_by_key(&byte, |(byte, _)| *byte) {
				Ok(child) => self.levels[at].children[child].1,
				Err(child) => {
					let next = self.levels.len();
					self.levels.push(Level::new());
					self.levels[at].children.insert(child, (byte, next));
					next
				}
			};
		}
		self.levels[at].value.get_or_insert_with(|| Box::new(init()))
	}

	fn child(&self, at: usize, byte: u8) -> Option<usize> {
		let children = &self.levels[at].children;
		let child = children.binary_search_by_key(&byte, |(byte, _)| *byte).ok()?;
		Some(children[child].1)
	}

	/// The level `key` ends at, if any value's key starts with it.
	fn find(&self, key: impl IntoIterator<Item = u8>) -> Option<usize> {
		if self.levels.is_empty() {
			return None;
		}
		key.into_iter().try_fold(Self::ROOT, |at, byte| self.child(at, byte))
	}

	/// The value keyed exactly `key`.
	fn get(&self, key: impl IntoIterator<Item = u8>) -> Option<&T> {
		self.levels[self.find(key)?].value.as_deref()
	}

	/// Visit each value whose key starts `key` and is at most `most` bytes, with its length.
	fn above<'t>(&'t self, key: impl IntoIterator<Item = u8>, most: usize, visit: &mut impl FnMut(usize, &'t T)) {
		if self.levels.is_empty() {
			return;
		}
		let mut key = key.into_iter();
		let mut at = Self::ROOT;
		for depth in 0..=most {
			if let Some(value) = &self.levels[at].value {
				visit(depth, value);
			}
			match key.next().and_then(|byte| self.child(at, byte)) {
				Some(next) => at = next,
				None => return,
			}
		}
	}

	/// Visit each value whose key starts with `key`.
	fn below<'t>(&'t self, key: impl IntoIterator<Item = u8>, visit: &mut impl FnMut(&'t T)) {
		if let Some(at) = self.find(key) {
			self.subtree(at, visit);
		}
	}

	/// Visit each value whose key starts `key` or starts with it.
	fn related<'t>(&'t self, key: impl IntoIterator<Item = u8>, visit: &mut impl FnMut(&'t T)) {
		if self.levels.is_empty() {
			return;
		}
		let mut at = Self::ROOT;
		for byte in key {
			// Strictly shorter keys here; the level `key` ends at holds the rest.
			if let Some(value) = &self.levels[at].value {
				visit(value);
			}
			match self.child(at, byte) {
				Some(next) => at = next,
				None => return,
			}
		}
		self.subtree(at, visit);
	}

	fn subtree<'t>(&'t self, at: usize, visit: &mut impl FnMut(&'t T)) {
		let mut stack = vec![at];
		while let Some(at) = stack.pop() {
			let level = &self.levels[at];
			if let Some(value) = &level.value {
				visit(value);
			}
			stack.extend(level.children.iter().map(|(_, child)| *child));
		}
	}
}

impl<T> Level<T> {
	fn new() -> Self {
		Self {
			value: None,
			children: Vec::new(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{IntersectionError, Patterns};

	fn pattern(text: &str) -> Pattern {
		text.parse().unwrap_or_else(|err| panic!("{text:?}: {err}"))
	}

	/// A deterministic generator, so a failure reproduces from its seed.
	struct Rng(u64);

	impl Rng {
		fn below(&mut self, n: usize) -> usize {
			self.0 = self
				.0
				.wrapping_mul(6364136223846793005)
				.wrapping_add(1442695040888963407);
			(self.0 >> 33) as usize % n
		}

		/// A pattern over a small alphabet, so members and queries meet often.
		fn pattern(&mut self) -> Pattern {
			const SEGMENTS: [&str; 18] = [
				"a", "b", "ab", "ba", "aab", "*", "*", "a*", "*b", "a*b", "ab*", "*ab", "aa*", "*bb", "é", "aé", "é*",
				"*é",
			];
			let len = self.below(5);
			let mut segments: Vec<&str> = (0..len).map(|_| SEGMENTS[self.below(SEGMENTS.len())]).collect();
			if self.below(3) == 0 {
				segments.insert(self.below(len + 1), "**");
			}
			pattern(&segments.join("/"))
		}

		fn patterns(&mut self) -> Vec<Pattern> {
			let n = self.below(24);
			(0..n).map(|_| self.pattern()).collect()
		}
	}

	/// The scans each indexed operation replaces.
	fn naive_reduce(patterns: &[Pattern]) -> Patterns {
		let mut out = Patterns::new();
		for pattern in patterns {
			out.insert(pattern.clone());
		}
		out
	}

	fn naive_intersect(a: &Patterns, b: &Patterns) -> Result<Patterns, IntersectionError> {
		let mut out = Patterns::new();
		for member in a.iter() {
			for candidate in b.iter() {
				for pattern in member.intersect(candidate)? {
					out.insert(pattern);
				}
			}
		}
		match out.len() > Pattern::MAX_INTERSECTIONS {
			true => Err(IntersectionError::TooManyPatterns),
			false => Ok(out),
		}
	}

	/// Hold every indexed operation to its scan, over `members` and `queries`.
	fn check(members: &[Pattern], queries: &[Pattern]) {
		let index = Index::new(members);
		for query in queries {
			let contains = members.iter().any(|member| member.contains(query));
			assert_eq!(index.contains(query), contains, "contains {query} in {members:?}");

			let mut overlapping: Vec<&Pattern> = index.overlapping(query);
			let mut expected: Vec<&Pattern> = members.iter().filter(|member| member.overlaps(query)).collect();
			overlapping.sort();
			expected.sort();
			expected.dedup();
			assert_eq!(overlapping, expected, "overlapping {query} in {members:?}");
		}

		let ours: Patterns = members.iter().cloned().collect();
		let theirs: Patterns = queries.iter().cloned().collect();
		assert_eq!(ours, naive_reduce(members), "reduce {members:?}");
		assert_eq!(
			ours.covers(&theirs),
			theirs
				.iter()
				.all(|query| ours.iter().any(|member| member.contains(query))),
			"{ours:?} covers {theirs:?}"
		);
		let mut extended = ours.clone();
		extended.extend(theirs.iter().cloned());
		assert_eq!(extended, naive_reduce(&[members, queries].concat()));
		assert_eq!(
			ours.intersect(&theirs),
			naive_intersect(&ours, &theirs),
			"{ours:?} intersect {theirs:?}"
		);
	}

	#[test]
	fn message_sized_segments() {
		// One segment may fill a whole message; indexing it must not recurse per byte.
		let long = "a".repeat(65_000);
		let half = "a".repeat(32_000);
		let members = [
			pattern(&long),
			pattern(&format!("{half}*{half}")),
			pattern(&format!("{long}/b")),
		];
		let set: Patterns = members.iter().cloned().collect();
		assert_eq!(set, Patterns::from_iter([members[1].clone(), members[2].clone()]));
		assert!(set.covers(&Patterns::from(pattern(&long))));
		assert!(set.intersect(&Patterns::from(pattern(&format!("{half}*")))).is_ok());
	}

	#[test]
	fn random_matches_scan() {
		let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
		for _ in 0..300 {
			let members = rng.patterns();
			let queries = rng.patterns();
			check(&members, &queries);
		}
	}

	#[test]
	fn adversarial_matches_scan() {
		let shapes: [fn(usize) -> String; 8] = [
			// Two specificity tiers that never contain each other.
			|i| match i % 2 {
				0 => format!("room{i}"),
				_ => format!("room{i}/x"),
			},
			// Partials of one literal that never contain each other: the prefix grows as
			// the suffix shrinks.
			|i| format!("{}*{}", "a".repeat(i % 9), "a".repeat(8 - i % 9)),
			// Literals those partials cover, or nearly.
			|i| "a".repeat(8 + i % 4),
			// `*` or a literal per position, failing only at the end.
			|i| {
				let head: Vec<&str> = (0..5).map(|bit| if i >> bit & 1 == 1 { "*" } else { "a" }).collect();
				format!("{}/end{}", head.join("/"), i % 7)
			},
			|i| format!("a/a/a/a/a/end{}", i % 9),
			// `**` with heads and tails of every length.
			|i| format!("{}**{}", "a/".repeat(i % 4), "/b".repeat(i / 4 % 4)),
			|i| format!("{}**{}", "*/".repeat(i % 3), "/*".repeat(i / 3 % 3)),
			|i| ["a"; 6][..i % 7].join("/"),
		];
		for member in shapes {
			for query in shapes {
				let members: Vec<Pattern> = (0..64).map(|i| pattern(&member(i))).collect();
				let queries: Vec<Pattern> = (0..64).map(|i| pattern(&query(i))).collect();
				check(&members, &queries);
			}
		}
	}
}
