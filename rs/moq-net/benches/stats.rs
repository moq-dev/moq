//! Drain benchmark for the stats registry: [`stats::Registry::report`] refilling
//! one reused report, swept over broadcasts and tiers so a per-drain cost that
//! grows faster than the entries it reports shows up as a slope.
//!
//! Run with `cargo bench -p moq-net --bench stats`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{announce, broadcast, origin, stats};

/// `(broadcasts, tiers)` shapes; every broadcast records under every tier.
const SHAPES: [(usize, usize); 4] = [(100, 1), (1_000, 1), (100, 10), (1_000, 10)];

/// A registry holding one live announce entry per `(broadcast, tier)`. The
/// handles are held: dropping them would close the entries and prune them.
struct Registry {
	registry: stats::Registry,
	_driver: origin::Driver,
	_publishers: Vec<broadcast::Producer>,
	_cursors: Vec<announce::Consumer>,
}

fn registry(broadcasts: usize, tiers: usize) -> Registry {
	let registry = stats::Registry::new(stats::Config::new());
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let publishers = (0..broadcasts)
		.map(|i| producer.publish(format!("room/{i}"), origin::Route::default()).unwrap())
		.collect();
	let cursors = (0..tiers)
		.map(|t| {
			let session = registry.tier(stats::Tier::new(format!("tier{t}"))).session("root");
			let mut cursor = producer.consume().with_stats(session).announced();
			// Each announce the cursor replays records under its tier.
			while cursor.next().now_or_never().flatten().is_some() {}
			cursor
		})
		.collect();
	Registry {
		registry,
		_driver: driver,
		_publishers: publishers,
		_cursors: cursors,
	}
}

fn report(c: &mut Criterion) {
	let mut group = c.benchmark_group("stats/report");
	for (broadcasts, tiers) in SHAPES {
		let fixture = registry(broadcasts, tiers);
		let mut report = stats::Report::default();
		fixture.registry.report(&mut report);
		assert_eq!(
			report.traffic.len(),
			broadcasts * tiers,
			"one entry per broadcast and tier"
		);

		group.bench_with_input(
			BenchmarkId::from_parameter(format!("{broadcasts}x{tiers}")),
			&fixture,
			|b, fixture| b.iter(|| fixture.registry.report(&mut report)),
		);
	}
	group.finish();
}

criterion_group!(benches, report);
criterion_main!(benches);
