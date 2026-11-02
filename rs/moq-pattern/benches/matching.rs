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
	group.finish();
}

criterion_group!(benches, matching, algebra);
criterion_main!(benches);
