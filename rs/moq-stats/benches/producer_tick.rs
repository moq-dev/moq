//! What one relay pays per stats tick to drain its registry and encode the
//! traffic tracks, swept over held paths, tiers, and the share of those paths
//! whose counters changed since the last tick.
//!
//! Idle paths stay in the plain snapshot for as long as the registry holds
//! them, so `heldB` grows with the table and is what nears the frame cap
//! (`cap%`).
//! `plainB` and `zB` are what this tick wrote (zero when nothing changed).
//! The plain side is a full snapshot; the compressed side is a merge-patch
//! delta. Time is the Criterion target `stats/tick`. The table is one warm-up
//! plus a few ticks, allocations included.
//!
//! Run `cargo bench -p moq-stats --features bench --bench producer_tick`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{announce, broadcast, origin, stats};
use moq_stats::produce::bench::{Driver, FrameBytes};

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

/// Ticks averaged into one table row, after a warm-up drain.
const TABLE_TICKS: u32 = 4;

/// Held paths. The last count is sized so its plain snapshot nears the cache cap; `cap%` shows how close.
const PATHS: [usize; 4] = [100, 1_000, 10_000, 50_000];

const TIERS: [usize; 2] = [1, 4];

/// Share of paths whose payload counter moves each tick. Zero stays idle.
const CHANGED: [usize; 3] = [0, 10, 100];

struct Relay {
	registry: stats::Registry,
	tiers: Vec<stats::Tier>,
	paths: Vec<String>,
	/// Bump every nth path. Zero bumps nothing.
	step: usize,
	driver: Driver,
	_origin: origin::Producer,
	_origin_driver: origin::Driver,
	_publishers: Vec<broadcast::Producer>,
	_cursors: Vec<announce::Consumer>,
	_sessions: Vec<stats::Session>,
}

impl Relay {
	fn build(paths: usize, tiers: usize, changed_percent: usize) -> Self {
		let registry = stats::Registry::new(
			stats::Config::new().with_exclude(moq_net::Pattern::subtree(".stats").expect("literal prefix")),
		);
		let (origin, origin_driver) = origin::Producer::new(origin::Config::default());
		let path_names: Vec<String> = (0..paths).map(|index| format!("room/{index:06}")).collect();
		let publishers: Vec<_> = path_names
			.iter()
			.map(|path| {
				origin
					.publish(path.as_str(), origin::Route::default())
					.expect("publish path")
			})
			.collect();
		let mut cursors = Vec::with_capacity(tiers);
		let mut sessions = Vec::with_capacity(tiers);
		let tier_labels: Vec<_> = (0..tiers)
			.map(|tier| {
				let tier = stats::Tier::new(format!("t{tier}"));
				let session = registry.tier(tier.clone()).session("root");
				let mut cursor = origin.consume().with_stats(session.clone()).announced();
				while cursor.next().now_or_never().flatten().is_some() {}
				cursors.push(cursor);
				sessions.push(session);
				tier
			})
			.collect();

		let mut report = stats::Report::default();
		registry.report(&mut report);
		assert_eq!(report.traffic.len(), paths * tiers, "one held entry per path and tier");

		let driver = Driver::new(registry.clone(), origin.clone()).expect("stats broadcast");
		Self {
			registry,
			tiers: tier_labels,
			paths: path_names,
			step: 100usize.checked_div(changed_percent).unwrap_or(0),
			driver,
			_origin: origin,
			_origin_driver: origin_driver,
			_publishers: publishers,
			_cursors: cursors,
			_sessions: sessions,
		}
	}

	/// Move the changed share's publisher byte counter, outside the timed tick.
	fn touch(&self) {
		if self.step == 0 {
			return;
		}
		for path in self.paths.iter().step_by(self.step) {
			for tier in &self.tiers {
				moq_net::fuzz::bump_publisher_bytes(&self.registry, path.as_str(), tier, 1);
			}
		}
	}
}

struct Row {
	allocs: usize,
	elapsed: Duration,
	bytes: FrameBytes,
}

/// Warm up once, then average a few ticks. The warm-up pays for track creation.
fn sample(relay: &mut Relay) -> Row {
	black_box(relay.driver.tick());
	let mut allocs = 0;
	let mut elapsed = Duration::ZERO;
	let mut bytes = FrameBytes::default();
	for _ in 0..TABLE_TICKS {
		relay.touch();
		ALLOCATIONS.store(0, Ordering::Relaxed);
		COUNTING.store(true, Ordering::Relaxed);
		let start = Instant::now();
		bytes = black_box(relay.driver.tick());
		elapsed += start.elapsed();
		COUNTING.store(false, Ordering::Relaxed);
		allocs += ALLOCATIONS.load(Ordering::Relaxed);
	}
	Row {
		allocs: allocs / TABLE_TICKS as usize,
		elapsed: elapsed / TABLE_TICKS,
		bytes,
	}
}

fn check_row(paths: usize, changed: usize, row: &Row) {
	let cap = moq_net::group::MAX_CACHE_BYTES;
	assert!(row.bytes.held > 0, "publisher snapshot missing");
	assert!(
		row.bytes.held as u64 <= cap,
		"plain frame exceeds the cache cap: {} > {cap}",
		row.bytes.held
	);
	// The snapshot keeps every held path, so a few paths cannot be a couple of braces.
	assert!(row.bytes.held > paths, "plain snapshot did not grow with held paths");
	if changed > 0 {
		assert!(row.bytes.plain > 0, "changed paths did not publish a plain frame");
		assert!(
			row.bytes.compressed > 0,
			"changed paths did not publish a compressed frame"
		);
	}
}

fn bench(c: &mut Criterion) {
	let cap = moq_net::group::MAX_CACHE_BYTES;
	println!("plain frame cap: {cap} bytes");
	println!("paths tiers changed% allocs/tick us/tick heldB cap% plainB zB");

	let mut group = c.benchmark_group("stats/tick");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for &changed in &CHANGED {
		for &tiers in &TIERS {
			for &paths in &PATHS {
				let id = BenchmarkId::from_parameter(format!("{paths}p_{tiers}t_{changed}pct"));
				group.throughput(Throughput::Elements((paths * tiers) as u64));
				// Criterion re-enters the closure per sample, so build and print the row
				// once, and only when this id runs (not under `--list` or a filter).
				let mut relay = None;
				group.bench_function(id, |b| {
					let relay = relay.get_or_insert_with(|| {
						let mut relay = Relay::build(paths, tiers, changed);
						let row = sample(&mut relay);
						let micros = row.elapsed.as_secs_f64() * 1e6;
						println!(
							"{paths:>5} {tiers:>5} {changed:>8} {allocs:>11} {micros:>8.1} {held:>8} {ratio:>4.0} {plain:>8} {z:>8}",
							allocs = row.allocs,
							held = row.bytes.held,
							ratio = row.bytes.held as f64 * 100.0 / cap as f64,
							plain = row.bytes.plain,
							z = row.bytes.compressed,
						);
						check_row(paths, changed, &row);
						relay
					});
					b.iter_custom(|iters| {
						let mut total = Duration::ZERO;
						for _ in 0..iters {
							relay.touch();
							let start = Instant::now();
							black_box(relay.driver.tick());
							total += start.elapsed();
						}
						total
					});
				});
			}
		}
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
