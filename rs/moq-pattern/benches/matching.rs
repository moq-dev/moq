//! Matching a pattern union, swept over union size and path depth, and whole-union operations.
//!
//! Run with `cargo bench -p moq-pattern --bench matching`.

use std::fmt;

use moq_pattern::{Pattern, Patterns, Prefix, Prefixes};

/// Matching one path against a union of literal paths.
mod pattern_matching {
	use std::fmt;

	use moq_pattern::{Pattern, Patterns};
	use swole::bench;

	/// A union's size and its paths' depth, named `<count>patterns-<depth>segments`.
	#[derive(Clone, Copy)]
	struct Case {
		count: usize,
		depth: usize,
	}

	impl Case {
		fn path(self, tenant: String) -> String {
			let tail = vec!["node"; self.depth - 1].join("/");
			if tail.is_empty() {
				tenant
			} else {
				format!("{tenant}/{tail}")
			}
		}

		fn patterns(self) -> Patterns {
			let patterns: Patterns = (0..self.count)
				.map(|index| self.path(format!("tenant{index:03}")).parse::<Pattern>().unwrap())
				.collect();
			assert_eq!(patterns.len(), self.count);
			patterns
		}
	}

	impl fmt::Display for Case {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "{}patterns-{}segments", self.count, self.depth)
		}
	}

	fn cases() -> Vec<Case> {
		[1, 16, 256]
			.into_iter()
			.flat_map(|count| [1, 8, 32].map(|depth| Case { count, depth }))
			.collect()
	}

	/// The union and a path matching its last member.
	fn last(case: Case) -> (Patterns, String) {
		let patterns = case.patterns();
		let input = case.path(format!("tenant{:03}", case.count - 1));
		assert!(patterns.matches(&input));
		(patterns, input)
	}

	/// The union and a path matching none of it.
	fn other(case: Case) -> (Patterns, String) {
		let patterns = case.patterns();
		let input = case.path("other".into());
		assert!(!patterns.matches(&input));
		(patterns, input)
	}

	#[bench(args = cases(), setup = last)]
	fn last_match((patterns, input): &(Patterns, String)) -> bool {
		patterns.matches(input)
	}

	#[bench(args = cases(), setup = other)]
	fn miss((patterns, input): &(Patterns, String)) -> bool {
		patterns.matches(input)
	}
}

/// Whole-union operations, swept over member kind and the size of each union.
///
/// `AUTH_OK` carries as many patterns as the peer chooses, so none of these may grow
/// with the product of two unions' sizes. Literals share one specificity tier; partials
/// share another; two-tier pairs each one-segment literal with a two-segment literal
/// under the same first segment, so the tiers meet on every shared prefix.
///
/// The pairwise operations sweep the held union (indexed) and the queried union
/// independently, so a cost that follows either one shows up as its own slope. The
/// unions never contain each other's members, so no query ends early. `covers` must
/// succeed on every query to visit them all: per kind it queries a subset of the held
/// union, and `covers-subtree` holds `sNNNN/**` members that cover any number of
/// literals beneath them, so it sweeps both sizes.
///
/// The [`Shape`] cases sweep one peer grant's encoded size up to a whole `AUTH_OK`:
/// `collect` reduces it as decoding does, and `covers` compares it against a previous
/// grant as a regrant does. Their throughput is bytes, so a cost that grows faster than
/// the message shows as falling throughput.
mod pattern_algebra {
	use moq_pattern::{Pattern, Patterns, Prefix, Prefixes};
	use swole::bench;

	use super::{Grant, Pair, Sizes, Union, grant_cases, kinds, pairs, prefix_grant, sizes, subtree_pairs, wire};

	fn inputs(case: Union) -> Vec<Pattern> {
		case.kind.union("room", case.count).into_iter().collect()
	}

	#[bench(args = kinds(), setup = inputs, items = |case: Union| case.count)]
	fn collect(inputs: &[Pattern]) -> usize {
		inputs.iter().cloned().collect::<Patterns>().len()
	}

	/// Our union of `held` and their union of `queried`, disjoint.
	fn unions(case: Pair) -> (Patterns, Patterns) {
		(
			case.kind.union("room", case.held),
			case.kind.union("other", case.queried),
		)
	}

	#[bench(args = pairs(false), setup = unions, items = Pair::total)]
	fn extend((ours, theirs): &(Patterns, Patterns)) -> usize {
		let mut union = ours.clone();
		union.extend(theirs.iter().cloned());
		union.len()
	}

	#[bench(args = pairs(false), setup = unions, items = Pair::total)]
	fn intersect((ours, theirs): &(Patterns, Patterns)) -> usize {
		ours.intersect(theirs).unwrap().len()
	}

	/// Our union of `held` and a subset of it of `queried`.
	fn subset(case: Pair) -> (Patterns, Patterns) {
		let ours = case.kind.union("room", case.held);
		let subset = case.kind.union("room", case.queried);
		assert!(ours.covers(&subset));
		(ours, subset)
	}

	#[bench(args = pairs(true), setup = subset, items = Pair::total)]
	fn covers((ours, subset): &(Patterns, Patterns)) -> bool {
		ours.covers(subset)
	}

	/// `held` subtrees and `queried` literals beneath them.
	fn subtrees(sizes: Sizes) -> (Patterns, Patterns) {
		let subtrees: Patterns = (0..sizes.held)
			.map(|index| format!("s{index:04}/**").parse::<Pattern>().unwrap())
			.collect();
		let literals: Patterns = (0..sizes.queried)
			.map(|index| {
				format!("s{:04}/room{index:04}", index % sizes.held)
					.parse::<Pattern>()
					.unwrap()
			})
			.collect();
		assert!(subtrees.covers(&literals));
		(subtrees, literals)
	}

	#[bench(args = subtree_pairs(), setup = subtrees, items = |sizes: Sizes| sizes.held + sizes.queried)]
	fn covers_subtree((subtrees, literals): &(Patterns, Patterns)) -> bool {
		subtrees.covers(literals)
	}

	/// A grant's members, them reduced, and the previous grant covering them.
	fn grant(case: Grant) -> (Vec<Pattern>, Patterns, Patterns) {
		let (message, previous) = case.shape.grant(case.bytes);
		let size = wire(&message);
		assert!(
			size <= case.bytes && size * 16 > case.bytes * 15,
			"{:?} fills {size} of {} bytes",
			case.shape,
			case.bytes
		);
		let message: Vec<Pattern> = message.iter().map(|text| text.parse().unwrap()).collect();
		let reduced: Patterns = message.iter().cloned().collect();
		assert_eq!(
			reduced.len(),
			message.len(),
			"{:?} members are incomparable",
			case.shape
		);
		assert!(previous.covers(&reduced));
		(message, reduced, previous)
	}

	#[bench(args = grant_cases(), setup = grant, bytes = Grant::size)]
	fn collect_shape((message, _, _): &(Vec<Pattern>, Patterns, Patterns)) -> usize {
		message.iter().cloned().collect::<Patterns>().len()
	}

	#[bench(args = grant_cases(), setup = grant, bytes = Grant::size)]
	fn covers_shape((_, reduced, previous): &(Vec<Pattern>, Patterns, Patterns)) -> bool {
		previous.covers(reduced)
	}

	/// The prefix grant's members, them reduced, and the previous grant of their parents.
	fn prefixes(bytes: usize) -> (Vec<Prefix>, Prefixes, Prefixes) {
		let (message, previous) = prefix_grant(bytes);
		let size = wire(&message);
		assert!(
			size <= bytes && size * 16 > bytes * 15,
			"prefix fills {size} of {bytes} bytes"
		);
		let message: Vec<Prefix> = message.iter().map(|text| text.parse().unwrap()).collect();
		let reduced: Prefixes = message.iter().cloned().collect();
		assert_eq!(reduced.len(), message.len(), "prefix members are disjoint");
		assert!(previous.covers(&reduced));
		(message, reduced, previous)
	}

	fn prefix_size(bytes: usize) -> usize {
		wire(&prefix_grant(bytes).0)
	}

	// The grant `AUTH_OK` carries: prefixes reduce in one sort and one sweep on any shape.
	#[bench(args = sizes(), setup = prefixes, bytes = prefix_size)]
	fn collect_prefix((message, _, _): &(Vec<Prefix>, Prefixes, Prefixes)) -> usize {
		message.iter().cloned().collect::<Prefixes>().len()
	}

	#[bench(args = sizes(), setup = prefixes, bytes = prefix_size)]
	fn covers_prefix((_, reduced, previous): &(Vec<Prefix>, Prefixes, Prefixes)) -> bool {
		previous.covers(reduced)
	}

	#[bench(args = sizes(), setup = prefixes, bytes = prefix_size)]
	fn intersect_prefix((_, reduced, previous): &(Vec<Prefix>, Prefixes, Prefixes)) -> usize {
		previous.intersect(reduced).len()
	}
}

/// What a union's members look like: literals share one specificity tier, partials another,
/// and two-tier pairs a one-segment literal with a two-segment one beneath it.
#[derive(Clone, Copy)]
enum Kind {
	Literal,
	Partial,
	TwoTier,
}

impl Kind {
	const ALL: [Self; 3] = [Self::Literal, Self::Partial, Self::TwoTier];

	fn union(self, name: &str, count: usize) -> Patterns {
		let union: Patterns = (0..count)
			.map(|index| match self {
				Self::Literal => format!("{name}{index:04}"),
				Self::Partial => format!("{name}{index:04}*tail"),
				Self::TwoTier if index % 2 == 0 => format!("{name}{:04}", index / 2),
				Self::TwoTier => format!("{name}{:04}/x", index / 2),
			})
			.map(|text| text.parse::<Pattern>().unwrap())
			.collect();
		assert_eq!(union.len(), count);
		union
	}
}

impl fmt::Display for Kind {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(match self {
			Self::Literal => "literal",
			Self::Partial => "partial",
			Self::TwoTier => "two-tier",
		})
	}
}

/// One union of a kind, named `<kind>/<count>`.
#[derive(Clone, Copy)]
struct Union {
	kind: Kind,
	count: usize,
}

impl fmt::Display for Union {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.kind, self.count)
	}
}

fn kinds() -> Vec<Union> {
	Kind::ALL
		.into_iter()
		.flat_map(|kind| [16, 256, 4096].map(|count| Union { kind, count }))
		.collect()
}

/// The held and queried unions' sizes, named `<held>x<queried>`.
#[derive(Clone, Copy)]
struct Sizes {
	held: usize,
	queried: usize,
}

impl fmt::Display for Sizes {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}", self.held, self.queried)
	}
}

/// The held and queried sizes swept, so a cost that follows either shows its own slope.
const PAIRS: [(usize, usize); 4] = [(16, 16), (4096, 16), (16, 4096), (4096, 4096)];

fn subtree_pairs() -> Vec<Sizes> {
	PAIRS.map(|(held, queried)| Sizes { held, queried }).into()
}

/// A held and a queried union of one kind, named `<kind>/<held>x<queried>`.
#[derive(Clone, Copy)]
struct Pair {
	kind: Kind,
	held: usize,
	queried: usize,
}

impl Pair {
	fn total(self) -> usize {
		self.held + self.queried
	}
}

impl fmt::Display for Pair {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}x{}", self.kind, self.held, self.queried)
	}
}

/// Every kind and size pair, only those whose queried union fits in the held one if `subset`.
fn pairs(subset: bool) -> Vec<Pair> {
	Kind::ALL
		.into_iter()
		.flat_map(|kind| PAIRS.map(|(held, queried)| Pair { kind, held, queried }))
		.filter(|pair| !subset || pair.queried <= pair.held)
		.collect()
}

/// The message sizes a grant fills.
fn sizes() -> [usize; 3] {
	[4096, 16384, MAX_MESSAGE]
}

/// A grant of one shape filling at most `bytes`, named `<shape>/<bytes>`.
#[derive(Clone, Copy)]
struct Grant {
	shape: Shape,
	bytes: usize,
}

impl Grant {
	/// The grant's encoded size.
	fn size(self) -> usize {
		wire(&self.shape.grant(self.bytes).0)
	}
}

impl fmt::Display for Grant {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.shape.name(), self.bytes)
	}
}

fn grant_cases() -> Vec<Grant> {
	[Shape::Mask, Shape::Chain]
		.into_iter()
		.flat_map(|shape| sizes().map(|bytes| Grant { shape, bytes }))
		.collect()
}

/// Distinct prefix members two segments beneath a shared parent, filling at most `bytes`
/// encoded and given in reverse order so collecting sorts them, with a previous grant of
/// their parents. Mirrors the prefix rows of `js/pattern/bench/algebra.ts`.
fn prefix_grant(bytes: usize) -> (Vec<String>, Prefixes) {
	let (mut texts, mut parents) = (Vec::new(), Vec::new());
	let mut size = 0;
	for index in 0.. {
		let parent = format!("room/{index:05}");
		let text = format!("{parent}/cam");
		size += varint(text.len()) + text.len();
		if varint(texts.len() + 1) + size > bytes {
			break;
		}
		texts.push(text);
		parents.push(parent.parse::<Prefix>().unwrap());
	}
	texts.reverse();
	(texts, parents.into_iter().collect())
}

/// The largest moq-lite control message, and so the largest `AUTH_OK`.
const MAX_MESSAGE: usize = u16::MAX as usize;

/// A QUIC varint's size, as `AUTH_OK` writes counts and lengths.
fn varint(value: usize) -> usize {
	match value {
		0..64 => 1,
		64..16384 => 2,
		_ => 4,
	}
}

/// A union's encoded size as `AUTH_OK` carries it: a count, then each text with its length.
fn wire(texts: &[String]) -> usize {
	varint(texts.len()) + texts.iter().map(|text| varint(text.len()) + text.len()).sum::<usize>()
}

/// A pattern union shaped against the union index, filling one message. No peer sends a
/// pattern union since `AUTH_OK` carries prefixes, so these measure local discovery filters.
///
/// Containment between patterns with wildcards reduces to Orthogonal Vectors, so some shape
/// costs more than linear time against any index; these are the two worst known against
/// this one. Every member is incomparable, so reduction keeps them all and visits each.
#[derive(Clone, Copy, Debug)]
enum Shape {
	/// `*`/`y` masks of equal weight, each ending in `y`, beside literal `y/…/y/xNNNN`
	/// queries that every mask matches but for the last segment.
	Mask,
	/// The correlated partials `a^i*z^(n-i)`, each sharing prefixes and suffixes with all
	/// the others.
	Chain,
}

impl Shape {
	/// Positions a mask chooses `*` or `y` for, half of them `*`.
	const FREE: usize = 14;

	fn name(self) -> &'static str {
		match self {
			Self::Mask => "mask",
			Self::Chain => "chain",
		}
	}

	/// The grant's texts, filling at most `bytes` encoded, and a previous grant covering
	/// every one, as a regrant compares them.
	fn grant(self, bytes: usize) -> (Vec<String>, Patterns) {
		let parse = |texts: &[String]| texts.iter().map(|text| text.parse::<Pattern>().unwrap()).collect();
		match self {
			Self::Mask => {
				let masks = (0u32..1 << Self::FREE)
					.filter(|bits| bits.count_ones() as usize == Self::FREE / 2)
					.map(|bits| {
						let segments = (0..Self::FREE).map(|at| if bits >> at & 1 == 1 { "*" } else { "y" });
						segments.chain(["y"]).collect::<Vec<_>>().join("/")
					});
				let queries = (0..).map(|index| format!("{}/x{index:04}", ["y"; Self::FREE].join("/")));
				let (mut kept, mut asked) = (Vec::new(), Vec::new());
				let mut texts = 0;
				for (mask, query) in masks.zip(queries) {
					texts += varint(mask.len()) + mask.len() + varint(query.len()) + query.len();
					if varint(2 * kept.len() + 2) + texts > bytes {
						break;
					}
					kept.push(mask);
					asked.push(query);
				}
				// `*/…/*/x*` holds every query, and a walk tries a literal before `*`, so each
				// query reaches it only after every mask.
				let previous = parse(&[kept.clone(), vec![format!("{}/x*", ["*"; Self::FREE].join("/"))]].concat());
				kept.extend(asked);
				(kept, previous)
			}
			Self::Chain => {
				let chain = |n: usize| -> Vec<String> {
					(1..n)
						.map(|i| format!("{}*{}", "a".repeat(i), "z".repeat(n - i)))
						.collect()
				};
				let n = (2..).take_while(|&n| wire(&chain(n)) <= bytes).last().unwrap();
				let texts = chain(n);
				let previous = parse(&texts);
				(texts, previous)
			}
		}
	}
}

fn main() {
	swole::main();
}
