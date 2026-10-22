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

/// Whole-union operations, swept over member count and member kind.
///
/// `AUTH_OK` carries as many patterns as the peer chooses, so none of these may grow
/// with the product of two unions' sizes. Literals share one specificity tier; partials
/// share another; two-tier alternates one- and two-segment literals, so every member of
/// the upper tier meets every member of the lower one. Each operation pairs unions that
/// never contain each other's members, so no query ends early.
fn algebra(c: &mut Criterion) {
	let mut group = c.benchmark_group("pattern-algebra");
	let union = |kind: &str, name: &str, count: usize| -> Vec<Pattern> {
		(0..count)
			.map(|index| match kind {
				"literal" => format!("{name}{index:04}"),
				"partial" => format!("{name}{index:04}*tail"),
				_ if index % 2 == 0 => format!("{name}{index:04}"),
				_ => format!("{name}{index:04}/x"),
			})
			.map(|text| text.parse().unwrap())
			.collect()
	};
	for count in [16, 256, 4096] {
		for kind in ["literal", "partial", "two-tier"] {
			let inputs = union(kind, "room", count);
			let ours: Patterns = inputs.iter().cloned().collect();
			let theirs: Patterns = union(kind, "other", count).into_iter().collect();
			assert_eq!(ours.len(), count);
			group.throughput(Throughput::Elements(count as u64));

			group.bench_function(BenchmarkId::new(format!("collect-{kind}"), count), |b| {
				b.iter(|| std::hint::black_box(inputs.iter().cloned().collect::<Patterns>().len()));
			});
			group.bench_function(BenchmarkId::new(format!("extend-{kind}"), count), |b| {
				b.iter(|| {
					let mut union = ours.clone();
					union.extend(theirs.iter().cloned());
					std::hint::black_box(union.len())
				});
			});
			group.bench_function(BenchmarkId::new(format!("covers-{kind}"), count), |b| {
				b.iter(|| std::hint::black_box(ours.covers(&ours)));
			});
			group.bench_function(BenchmarkId::new(format!("intersect-{kind}"), count), |b| {
				b.iter(|| std::hint::black_box(ours.intersect(&theirs).unwrap().len()));
			});
		}
	}
	group.finish();
}

criterion_group!(benches, matching, algebra);
criterion_main!(benches);
