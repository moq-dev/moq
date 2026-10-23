//! Drain benchmark for the stats registry: [`stats::Registry::report`] refilling
//! one reused report, swept over broadcasts, tiers, labels, sessions and connections
//! so a cost that grows faster than the entries it reports shows up as a slope.
//!
//! Run with `cargo bench -p moq-net --bench stats`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{announce, fuzz, origin, stats, transport};

/// `(broadcasts, tiers)` shapes; every broadcast records under every tier.
const SHAPES: [(usize, usize); 4] = [(100, 1), (1_000, 1), (100, 10), (1_000, 10)];

/// A registry holding one live announce entry per `(broadcast, tier)`. The
/// handles are held: dropping them would close the entries and prune them.
struct Registry {
	registry: stats::Registry,
	_driver: origin::Driver,
	_publishers: Vec<origin::Publication>,
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

fn request(c: &mut Criterion) {
	let mut group = c.benchmark_group("stats/request");
	for roots in [1, 100, 1000] {
		for tiers in [1, 10] {
			let registry = stats::Registry::new(stats::Config::new());
			let mut contexts = Vec::new();
			for tier in 0..tiers {
				let handle = registry.tier(stats::Tier::new(format!("tier{tier}")));
				for root in 0..roots {
					contexts.push(handle.request(format!("room/{root}")));
				}
			}
			let handle = registry.tier(stats::Tier::new(format!("tier{}", tiers - 1)));
			let root = format!("room/{}", roots - 1);
			group.bench_function(BenchmarkId::from_parameter(format!("{roots}x{tiers}")), |b| {
				b.iter(|| std::hint::black_box(handle.request(&root)));
			});
			std::hint::black_box(contexts);
		}
	}
	group.finish();
}

fn write(c: &mut Criterion) {
	let registry = stats::Registry::new(stats::Config::new());
	let tier = stats::Tier::default();
	let path = fuzz::path_meter(&registry, &tier, "room/live");
	let session = fuzz::session_meter(&registry, tier, "root", "room/live");
	c.bench_function("stats/write/path-bytes", |b| b.iter(|| path.bytes(1)));
	c.bench_function("stats/write/session-bytes", |b| b.iter(|| session.bytes(1)));
}

/// Include the live transport walk, independently swept over labels, sessions and connections.
fn session_report(c: &mut Criterion) {
	let mut group = c.benchmark_group("stats/session-report");
	for labels in [1, 100, 1_000] {
		for sessions in [1, 10] {
			for connections in [0, 1, 4] {
				let registry = stats::Registry::new(stats::Config::new());
				let handle = registry.tier(stats::Tier::default());
				let contexts: Vec<_> = (0..labels)
					.flat_map(|label| {
						let handle = handle.clone();
						(0..sessions).map(move |_| handle.session(format!("project/key/{label}")))
					})
					.collect();
				let wires: Vec<_> = contexts
					.iter()
					.flat_map(|session| {
						(0..connections).map(move |_| {
							let counters = transport::Wire::new();
							counters.set_sent_bytes(100);
							counters.set_received_bytes(200);
							session.wire(counters)
						})
					})
					.collect();
				let mut report = stats::Report::default();
				registry.report(&mut report);
				assert_eq!(report.sessions.len(), labels);
				for row in &report.sessions {
					assert_eq!(row.presence.sessions_started, sessions as u64);
					assert_eq!(row.presence.wire_tx, (sessions * connections * 100) as u64);
					assert_eq!(row.presence.wire_rx, (sessions * connections * 200) as u64);
				}
				group.throughput(Throughput::Elements((labels * sessions) as u64));
				group.bench_function(
					BenchmarkId::from_parameter(format!("{labels}x{sessions}/connections={connections}")),
					|b| b.iter(|| registry.report(&mut report)),
				);
				std::hint::black_box((contexts, wires));
			}
		}
	}
	group.finish();
}

/// Move every session on one label to another and back, as a revalidation that relabels
/// a whole project does. Per-session cost should stay flat as sessions and labels grow.
fn retag(c: &mut Criterion) {
	let mut group = c.benchmark_group("stats/retag");
	for labels in [1, 100] {
		for sessions in [1, 100, 10_000] {
			let registry = stats::Registry::new(stats::Config::new());
			let tier = stats::Tier::default();
			let handle = registry.tier(tier.clone());
			let mut contexts: Vec<_> = (0..labels)
				.flat_map(|label| {
					let handle = handle.clone();
					(0..sessions).map(move |_| handle.session(format!("project/key/{label}")))
				})
				.collect();
			let moving = contexts.split_off(contexts.len() - sessions);
			group.throughput(Throughput::Elements(sessions as u64 * 2));
			group.bench_function(BenchmarkId::from_parameter(format!("{labels}x{sessions}")), |b| {
				b.iter(|| {
					for session in moving.iter().rev() {
						session.retag(tier.clone(), "project/moved");
					}
					for session in moving.iter().rev() {
						session.retag(tier.clone(), format!("project/key/{}", labels - 1));
					}
				});
			});
			std::hint::black_box((contexts, moving));
		}
	}
	group.finish();
}

criterion_group!(benches, report, request, write, session_report, retag);
criterion_main!(benches);
