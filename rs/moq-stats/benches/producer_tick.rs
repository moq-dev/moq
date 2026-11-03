//! What one relay pays per stats tick to drain its registry and encode its
//! group track, swept over held paths, tiers, and the share of those paths
//! whose counters changed since the last tick.
//!
//! Idle paths stay in the group's snapshot for as long as the registry holds
//! them, so `heldB` grows with the table and is what nears the frame cap
//! (`cap%`).
//! `wroteB` is what this tick wrote: a merge-patch delta, or a snapshot when
//! the tick opened a group, and zero when nothing changed. Time is the
//! Criterion target `stats/tick`. The table is one warm-up plus a few ticks,
//! allocations included.
//!
//! `stats/presence` sweeps the same tick over connected labels and the
//! presence tracks held for them.
//!
//! `stats/prefix` sweeps it over held paths, path depth, and held prefix
//! tracks: each row walks its ancestors once a prefix is held, so the cost
//! grows with rows times depth, plus each held track's encode.
//!
//! `stats/groups` sweeps a depth-1 producer over held paths and the groups
//! they spread across, each group one track: the rows cost the same however
//! they split, so a slope in groups is per-track overhead.
//!
//! Run `cargo bench -p moq-stats --features bench --bench producer_tick`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{announce, origin, stats};
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

/// Held paths. The last count is sized so its one-tier snapshot nears the cache cap; `cap%` shows
/// how close. Every tier shares the one group's snapshot, so more tiers stop below it.
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
	_publishers: Vec<origin::Publication>,
	_cursors: Vec<announce::Consumer>,
	_sessions: Vec<stats::Session>,
	_held: Vec<moq_net::track::Subscriber>,
}

impl Relay {
	/// `depth` is each path's segment count, at least 2. With `groups`, the
	/// paths spread over that many first segments and the driver publishes each
	/// as its own group track; without, every path shares one.
	fn build(paths: usize, tiers: usize, changed_percent: usize, depth: usize, groups: Option<usize>) -> Self {
		let registry = stats::Registry::new(
			stats::Config::new().with_exclude(moq_net::Pattern::subtree(".stats").expect("literal prefix")),
		);
		let (origin, origin_driver) = origin::Producer::new(origin::Config::default());
		let nested = "/s".repeat(depth - 2);
		let path_names: Vec<String> = (0..paths)
			.map(|index| match groups {
				Some(groups) => format!("g{:04}/{index:06}{nested}", index % groups),
				None => format!("room/{index:06}{nested}"),
			})
			.collect();
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
				let tier = stats::Tier::new(moq_net::Path::new(&format!("t{tier}")).unwrap());
				let session = registry.tier(tier.clone()).session(moq_net::Path::from_static("root"));
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

		let depth = if groups.is_some() { 1 } else { 0 };
		let driver = Driver::new(registry.clone(), origin.clone(), depth).expect("stats broadcast");
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
			_held: Vec::new(),
		}
	}

	/// Hold the first `held` paths' `room/<index>` prefix tracks.
	fn hold(&mut self, held: usize) {
		for index in 0..held {
			let sub = self.driver.hold_prefix(&format!("room/{index:06}"));
			self._held.push(sub);
		}
	}

	/// Move the changed share's publisher byte counter, outside the timed tick.
	fn touch(&self) {
		if self.step == 0 {
			return;
		}
		for path in self.paths.iter().step_by(self.step) {
			for tier in &self.tiers {
				moq_net::fuzz::bump_publisher_bytes(
					&self.registry,
					moq_net::Path::new(path.as_str()).unwrap(),
					tier,
					1,
				);
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
	assert!(row.bytes.held > 0, "group snapshot missing");
	assert!(
		row.bytes.held as u64 <= cap,
		"group frame exceeds the cache cap: {} > {cap}",
		row.bytes.held
	);
	// The snapshot keeps every held path, so a few paths cannot be a couple of braces.
	assert!(row.bytes.held > paths, "group snapshot did not grow with held paths");
	if changed > 0 {
		assert!(row.bytes.wrote > 0, "changed paths did not publish a frame");
	}
}

fn bench(c: &mut Criterion) {
	let cap = moq_net::group::MAX_CACHE_BYTES;
	println!("plain frame cap: {cap} bytes");
	println!("paths tiers changed% allocs/tick us/tick heldB cap% wroteB");

	let mut group = c.benchmark_group("stats/tick");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for &changed in &CHANGED {
		for &tiers in &TIERS {
			for &paths in &PATHS {
				if tiers > 1 && paths == PATHS[PATHS.len() - 1] {
					continue;
				}
				let id = BenchmarkId::from_parameter(format!("{paths}p_{tiers}t_{changed}pct"));
				group.throughput(Throughput::Elements((paths * tiers) as u64));
				// Criterion re-enters the closure per sample, so build and print the row
				// once, and only when this id runs (not under `--list` or a filter).
				let mut relay = None;
				group.bench_function(id, |b| {
					let relay = relay.get_or_insert_with(|| {
						let mut relay = Relay::build(paths, tiers, changed, 2, None);
						let row = sample(&mut relay);
						let micros = row.elapsed.as_secs_f64() * 1e6;
						println!(
							"{paths:>5} {tiers:>5} {changed:>8} {allocs:>11} {micros:>8.1} {held:>8} {ratio:>4.0} {wrote:>8}",
							allocs = row.allocs,
							held = row.bytes.held,
							ratio = row.bytes.held as f64 * 100.0 / cap as f64,
							wrote = row.bytes.wrote,
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

/// Connected labels, one session each.
const LABELS: [usize; 2] = [100, 10_000];

/// Labels whose presence track a reader holds.
const HELD: [usize; 3] = [0, 100, 1_000];

/// A relay with sessions under many labels, some of them held.
struct Presence {
	driver: Driver,
	_registry: stats::Registry,
	_origin: origin::Producer,
	_origin_driver: origin::Driver,
	_sessions: Vec<stats::Session>,
	_wire: Vec<stats::Wire>,
	_held: Vec<moq_net::track::Subscriber>,
}

impl Presence {
	fn build(labels: usize, held: usize) -> Self {
		let registry = stats::Registry::new(stats::Config::new());
		let (origin, origin_driver) = origin::Producer::new(origin::Config::default());
		let names: Vec<String> = (0..labels).map(|index| format!("room/{index:05}")).collect();
		let sessions: Vec<_> = names
			.iter()
			.map(|label| {
				registry
					.tier(stats::Tier::default())
					.session(moq_net::Path::new(label.as_str()).unwrap())
			})
			.collect();
		// One set of wire counters per connection, independently of held presence tracks.
		let wire = sessions
			.iter()
			.map(|session| session.wire(moq_net::transport::Wire::new()))
			.collect();
		let mut driver = Driver::new(registry.clone(), origin.clone(), 0).expect("stats broadcast");
		driver.tick();
		let held = names
			.iter()
			.take(held)
			.map(|label| driver.hold_presence(label))
			.collect();
		Self {
			driver,
			_registry: registry,
			_origin: origin,
			_origin_driver: origin_driver,
			_sessions: sessions,
			_wire: wire,
			_held: held,
		}
	}
}

/// What holding presence tracks costs a tick, swept over connected labels and
/// held labels: each session row looks its label up once, and each held track
/// is summed and encoded once.
fn presence(c: &mut Criterion) {
	println!("labels held allocs/tick us/tick");

	let mut group = c.benchmark_group("stats/presence");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for &labels in &LABELS {
		for &held in HELD.iter().filter(|&&held| held <= labels) {
			let id = BenchmarkId::from_parameter(format!("{labels}l_{held}h"));
			group.throughput(Throughput::Elements(labels as u64));
			let mut relay = None;
			group.bench_function(id, |b| {
				let relay = relay.get_or_insert_with(|| {
					let mut relay = Presence::build(labels, held);
					black_box(relay.driver.tick());
					ALLOCATIONS.store(0, Ordering::Relaxed);
					COUNTING.store(true, Ordering::Relaxed);
					let start = Instant::now();
					for _ in 0..TABLE_TICKS {
						black_box(relay.driver.tick());
					}
					let micros = start.elapsed().as_secs_f64() * 1e6 / TABLE_TICKS as f64;
					COUNTING.store(false, Ordering::Relaxed);
					let allocs = ALLOCATIONS.load(Ordering::Relaxed) / TABLE_TICKS as usize;
					println!("{labels:>5} {held:>4} {allocs:>11} {micros:>8.1}");
					relay
				});
				b.iter_custom(|iters| {
					let start = Instant::now();
					for _ in 0..iters {
						black_box(relay.driver.tick());
					}
					start.elapsed()
				});
			});
		}
	}
	group.finish();
}

const PREFIX_PATHS: [usize; 2] = [1_000, 10_000];

/// Segments per path.
const PREFIX_DEPTH: [usize; 2] = [2, 8];

/// Prefix tracks a reader holds.
const PREFIX_HELD: [usize; 3] = [0, 10, 100];

/// What holding prefix tracks costs a tick, with a tenth of the paths moving.
fn prefix(c: &mut Criterion) {
	println!("paths depth held allocs/tick us/tick");

	let mut group = c.benchmark_group("stats/prefix");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for &paths in &PREFIX_PATHS {
		for &depth in &PREFIX_DEPTH {
			for &held in &PREFIX_HELD {
				let id = BenchmarkId::from_parameter(format!("{paths}p_{depth}d_{held}h"));
				group.throughput(Throughput::Elements(paths as u64));
				let mut relay = None;
				group.bench_function(id, |b| {
					let relay = relay.get_or_insert_with(|| {
						let mut relay = Relay::build(paths, 1, 10, depth, None);
						black_box(relay.driver.tick());
						relay.hold(held);
						let row = sample(&mut relay);
						let micros = row.elapsed.as_secs_f64() * 1e6;
						println!(
							"{paths:>5} {depth:>5} {held:>4} {allocs:>11} {micros:>8.1}",
							allocs = row.allocs
						);
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

const GROUP_PATHS: [usize; 2] = [1_000, 10_000];

/// Groups the paths spread across, each a track.
const GROUPS: [usize; 3] = [1, 100, 1_000];

/// What spreading paths over group tracks costs a tick, with a tenth of the
/// paths moving.
fn groups(c: &mut Criterion) {
	println!("paths groups allocs/tick us/tick");

	let mut group = c.benchmark_group("stats/groups");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for &paths in &GROUP_PATHS {
		for &groups in GROUPS.iter().filter(|&&groups| groups <= paths) {
			let id = BenchmarkId::from_parameter(format!("{paths}p_{groups}g"));
			group.throughput(Throughput::Elements(paths as u64));
			let mut relay = None;
			group.bench_function(id, |b| {
				let relay = relay.get_or_insert_with(|| {
					let mut relay = Relay::build(paths, 1, 10, 2, Some(groups));
					let row = sample(&mut relay);
					let micros = row.elapsed.as_secs_f64() * 1e6;
					println!("{paths:>5} {groups:>6} {allocs:>11} {micros:>8.1}", allocs = row.allocs);
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
	group.finish();
}

criterion_group!(benches, bench, presence, prefix, groups);
criterion_main!(benches);
