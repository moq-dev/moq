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

criterion_group!(benches, matching);
criterion_main!(benches);
