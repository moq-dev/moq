//! Drain benchmark for the stats registry: [`stats::Registry::report`] refilling
//! one reused report, swept over broadcasts and tiers so a per-drain cost that
//! grows faster than the entries it reports shows up as a slope, and over
//! broadcasts and egress subscriptions per broadcast, since every drain also
//! samples each subscription's lag.
//!
//! Run with `cargo bench -p moq-net --bench stats`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Timestamp, announce, broadcast, origin, stats, track};

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

/// `(broadcasts, subscriptions per broadcast)` shapes for the lag sampler.
const SUBSCRIBED: [(usize, usize); 4] = [(100, 1), (1_000, 1), (100, 10), (1_000, 10)];

/// A registry with `subscriptions` tagged egress subscriptions on each of
/// `broadcasts` live broadcasts, each track holding one frame.
struct Subscribed {
	registry: stats::Registry,
	_driver: origin::Driver,
	_publishers: Vec<(broadcast::Producer, track::Producer)>,
	_subscribers: Vec<track::Subscriber>,
}

fn subscribed(broadcasts: usize, subscriptions: usize) -> Subscribed {
	let registry = stats::Registry::new(stats::Config::new());
	let (producer, mut driver) = origin::Producer::new(origin::Config::default());
	let waiter = kio::Waiter::noop();
	let mut publishers = Vec::new();
	let mut subscribers = Vec::new();
	for i in 0..broadcasts {
		let path = format!("room/{i}");
		let broadcast = producer.publish(path.as_str(), origin::Route::default()).unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, vec![0u8; 100]).unwrap();
		group.finish().unwrap();
		for _ in 0..subscriptions {
			let session = registry.tier(stats::Tier::default()).session("root");
			let pending = producer.consume().with_stats(session).request_broadcast(path.as_str());
			driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			let consumer = pending.now_or_never().expect("resolves once driven").unwrap();
			let pending = consumer.track("video").unwrap().subscribe(None);
			driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			subscribers.push(pending.now_or_never().expect("subscribes once driven").unwrap());
		}
		publishers.push((broadcast, track));
	}
	Subscribed {
		registry,
		_driver: driver,
		_publishers: publishers,
		_subscribers: subscribers,
	}
}

fn sample(c: &mut Criterion) {
	let mut group = c.benchmark_group("stats/sample");
	for (broadcasts, subscriptions) in SUBSCRIBED {
		let fixture = subscribed(broadcasts, subscriptions);
		let mut report = stats::Report::default();
		fixture.registry.report(&mut report);
		let subscribed: u64 = report.traffic.iter().map(|e| e.publisher.subscriptions_started).sum();
		assert_eq!(subscribed, (broadcasts * subscriptions) as u64, "every subscription is tagged");

		group.bench_with_input(
			BenchmarkId::from_parameter(format!("{broadcasts}x{subscriptions}")),
			&fixture,
			|b, fixture| b.iter(|| fixture.registry.report(&mut report)),
		);
	}
	group.finish();
}

criterion_group!(benches, report, sample);
criterion_main!(benches);
