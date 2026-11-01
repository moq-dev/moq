//! Matching a pattern union, swept over union size and path depth.
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_pattern::{Pattern, Patterns};

fn matching(c: &mut Criterion) {
	let mut group = c.benchmark_group("pattern-matching");
	for count in [1, 16, 256] {
		for depth in [1, 8, 32] {
			let tail = vec!["node"; depth - 1].join("/");
			let path = |tenant: String| {
				if tail.is_empty() {
					tenant
				} else {
					format!("{tenant}/{tail}")
				}
			};
			let patterns: Patterns = (0..count)
				.map(|index| path(format!("tenant{index:03}")).parse::<Pattern>().unwrap())
				.collect();
			assert_eq!(patterns.len(), count);
			for (case, input, expected) in [
				("last-match", path(format!("tenant{:03}", count - 1)), true),
				("miss", path("other".into()), false),
			] {
				assert_eq!(patterns.matches(&input), expected);
				group.throughput(Throughput::Elements(1));
				group.bench_function(
					BenchmarkId::new(case, format!("{count}patterns-{depth}segments")),
					|b| {
						b.iter(|| std::hint::black_box(patterns.matches(std::hint::black_box(&input))));
					},
				);
			}
		}
	}
	group.finish();
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
fn algebra(c: &mut Criterion) {
	let mut group = c.benchmark_group("pattern-algebra");
	let union = |kind: &str, name: &str, count: usize| -> Patterns {
		let union: Patterns = (0..count)
			.map(|index| match kind {
				"literal" => format!("{name}{index:04}"),
				"partial" => format!("{name}{index:04}*tail"),
				_ if index % 2 == 0 => format!("{name}{:04}", index / 2),
				_ => format!("{name}{:04}/x", index / 2),
			})
			.map(|text| text.parse::<Pattern>().unwrap())
			.collect();
		assert_eq!(union.len(), count);
		union
	};
	for kind in ["literal", "partial", "two-tier"] {
		for count in [16, 256, 4096] {
			let inputs: Vec<Pattern> = union(kind, "room", count).into_iter().collect();
			group.throughput(Throughput::Elements(count as u64));
			group.bench_function(BenchmarkId::new(format!("collect-{kind}"), count), |b| {
				b.iter(|| std::hint::black_box(inputs.iter().cloned().collect::<Patterns>().len()));
			});
		}
		for (held, queried) in [(16, 16), (4096, 16), (16, 4096), (4096, 4096)] {
			let ours = union(kind, "room", held);
			let theirs = union(kind, "other", queried);
			let sizes = format!("{held}x{queried}");
			group.throughput(Throughput::Elements((held + queried) as u64));

			group.bench_function(BenchmarkId::new(format!("extend-{kind}"), &sizes), |b| {
				b.iter(|| {
					let mut union = ours.clone();
					union.extend(theirs.iter().cloned());
					std::hint::black_box(union.len())
				});
			});
			group.bench_function(BenchmarkId::new(format!("intersect-{kind}"), &sizes), |b| {
				b.iter(|| std::hint::black_box(ours.intersect(&theirs).unwrap().len()));
			});
			if queried <= held {
				let subset = union(kind, "room", queried);
				assert!(ours.covers(&subset));
				group.bench_function(BenchmarkId::new(format!("covers-{kind}"), &sizes), |b| {
					b.iter(|| std::hint::black_box(ours.covers(&subset)));
				});
			}
		}
	}
	for (held, queried) in [(16, 16), (4096, 16), (16, 4096), (4096, 4096)] {
		let subtrees: Patterns = (0..held)
			.map(|index| format!("s{index:04}/**").parse::<Pattern>().unwrap())
			.collect();
		let literals: Patterns = (0..queried)
			.map(|index| {
				format!("s{:04}/room{index:04}", index % held)
					.parse::<Pattern>()
					.unwrap()
			})
			.collect();
		assert!(subtrees.covers(&literals));
		group.throughput(Throughput::Elements((held + queried) as u64));
		group.bench_function(BenchmarkId::new("covers-subtree", format!("{held}x{queried}")), |b| {
			b.iter(|| std::hint::black_box(subtrees.covers(&literals)));
		});
	}
	// Each iteration reduces a whole message, so fewer samples keep the sweep short.
	group.sample_size(10);
	for shape in [Shape::Mask, Shape::Chain] {
		for bytes in [4096, 16384, MAX_MESSAGE] {
			let (message, previous) = shape.grant(bytes);
			let size = wire(&message);
			assert!(
				size <= bytes && size * 16 > bytes * 15,
				"{shape:?} fills {size} of {bytes} bytes"
			);
			let message: Vec<Pattern> = message.iter().map(|text| text.parse().unwrap()).collect();
			let reduced: Patterns = message.iter().cloned().collect();
			assert_eq!(reduced.len(), message.len(), "{shape:?} members are incomparable");
			assert!(previous.covers(&reduced));
			group.throughput(Throughput::Bytes(size as u64));
			group.bench_function(BenchmarkId::new(format!("collect-{}", shape.name()), bytes), |b| {
				b.iter(|| std::hint::black_box(message.iter().cloned().collect::<Patterns>().len()));
			});
			group.bench_function(BenchmarkId::new(format!("covers-{}", shape.name()), bytes), |b| {
				b.iter(|| std::hint::black_box(previous.covers(&reduced)));
			});
		}
	}
	group.finish();
}

/// The largest moq-lite control message, and so the largest `AUTH_OK`.
const MAX_MESSAGE: usize = u16::MAX as usize;

/// A QUIC varint's size, as `AUTH_OK` writes counts and lengths.
fn varint(value: usize) -> usize {
	match value {
		..64 => 1,
		..16384 => 2,
		_ => 4,
	}
}

/// A union's encoded size as `AUTH_OK` carries it: a count, then each text with its length.
fn wire(texts: &[String]) -> usize {
	varint(texts.len()) + texts.iter().map(|text| varint(text.len()) + text.len()).sum::<usize>()
}

/// A peer grant shaped against the union index, filling one message.
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

criterion_group!(benches, matching, algebra);
criterion_main!(benches);
