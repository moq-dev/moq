//! Producer-to-subscriber fanout benchmarks for the track model.
//!
//! The group benchmarks isolate per-frame storage. These benchmarks include the
//! track cache and subscription cursors around that storage: append a complete
//! group, notify every subscriber, and hand the cached group to each cursor.
//!
//! `track_aborted_scan` isolates the other half of delivery: how much a cached
//! prefix of aborted groups costs the scan that has to walk past it.
//!
//! `track_gc` is the cache's clock-driven half: the per-poll [`cache::Pool::gc`]
//! call every origin driver makes, and the due expiry pass it runs over every track.
//!
//! `track_subscriber_churn` measures viewers joining and leaving beside steady ones:
//! each departure wakes the producer's aggregate poll, which walks the subscription
//! list, so departed entries left in the list show up as a slope over churn.
//! `track_subscriber_churn_after_peak` runs the same churn after a departed audience,
//! so entries walked in proportion to the old peak show up as a slope over it.
//!
//! `track_subscriber_join` measures a burst of viewers joining one track: each join
//! registers in the subscription list, so any per-join walk of it shows up as a slope.
//!
//! Run with `cargo bench -p moq-net --bench track`.

use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Error, Timestamp, broadcast, cache, track};

/// Fanout sizes spanning a direct viewer, a small room, and a large room.
const FANOUT: [usize; 4] = [1, 8, 64, 512];

/// Aborted-prefix sizes spanning a cache hit through a long stale scan.
const ABORTED: [usize; 4] = [1, 8, 64, 512];

/// Small shared payload so the benchmark measures model overhead, not allocation.
const PAYLOAD: usize = 64;

/// Small enough to reach steady-state eviction during Criterion warm-up.
const CACHE_CAPACITY: u64 = 64 * 1024;

/// Steady viewers that stay subscribed while others churn.
const STEADY: [usize; 3] = [1, 8, 64];

/// Viewers that join and leave, one at a time, during one measured run.
const CHURN: [usize; 3] = [8, 64, 512];

/// Viewers that join at once and stay, during one measured run.
const JOIN: [usize; 3] = [64, 1024, 16384];

/// Viewers that joined and left before the churn, leaving the list room for that many.
const PEAK: [usize; 3] = [0, 1024, 16384];

/// Concurrent publishers sharing one relay-style cache pool.
const WRITERS: [usize; 4] = [1, 2, 4, 8];

/// Tracks sharing one expiring pool: the table a due collection pass walks.
const SWEPT: [usize; 4] = [1, 64, 1_024, 16_384];

/// Keeps the ownership chain alive around the track and its subscribers.
struct Fanout {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	subscribers: Vec<track::Subscriber>,
	waiters: Vec<kio::Waiter>,
	payload: Bytes,
}

impl Fanout {
	fn new(subscribers: usize) -> Self {
		let mut info = broadcast::Info::default();
		let config = cache::Config::default()
			.with_capacity(CACHE_CAPACITY)
			.with_expiry(cache::DEFAULT_EXPIRY);
		info.pool = cache::Pool::new(config);
		let broadcast = broadcast::Producer::new(info);
		let track = broadcast.create_track("bench", None).unwrap();
		let mut subscribers: Vec<_> = (0..subscribers).map(|_| track.subscribe(None)).collect();
		let waiters: Vec<_> = (0..subscribers.len()).map(|_| kio::Waiter::noop()).collect();

		for (subscriber, waiter) in subscribers.iter_mut().zip(&waiters) {
			assert!(matches!(subscriber.poll_recv_group(waiter), Poll::Pending));
		}

		Self {
			_broadcast: broadcast,
			track,
			subscribers,
			waiters,
			payload: Bytes::from(vec![0; PAYLOAD]),
		}
	}

	/// Append one finished group and deliver its handle to every subscriber.
	fn cycle(&mut self) {
		let mut group = self.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, self.payload.clone()).unwrap();
		group.finish().unwrap();

		for (subscriber, waiter) in self.subscribers.iter_mut().zip(&self.waiters) {
			let group = match subscriber.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => group,
				_ => unreachable!("a completed group must be ready"),
			};
			black_box(group);
			assert!(matches!(subscriber.poll_recv_group(waiter), Poll::Pending));
		}
	}
}

/// A cached aborted prefix followed by one live group.
struct AbortedScan {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	waiter: kio::Waiter,
}

impl AbortedScan {
	fn new(aborted: usize) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", None).unwrap();
		let mut stale = Vec::with_capacity(aborted);

		for _ in 0..aborted {
			stale.push(track.append_group().unwrap());
		}
		track.append_group().unwrap().finish().unwrap();
		for group in stale {
			group.abort(Error::Cancel).unwrap();
		}

		Self {
			_broadcast: broadcast,
			track,
			waiter: kio::Waiter::noop(),
		}
	}

	/// Walk the aborted prefix to reach the trailing live group.
	fn scan(&self) {
		let mut subscriber = self.track.subscribe(None);
		let group = match subscriber.poll_recv_group(&self.waiter) {
			Poll::Ready(Ok(Some(group))) => group,
			_ => unreachable!("the trailing live group must be ready"),
		};
		black_box(group);
	}
}

/// Live tracks with one cached group each, sharing an expiring pool.
///
/// Each track's only group is its live latest, which expiry never reclaims, so
/// every pass walks the same table instead of emptying it.
struct Swept {
	_broadcast: broadcast::Producer,
	_tracks: Vec<track::Producer>,
	pool: cache::Pool,
	now: Instant,
}

impl Swept {
	fn new(tracks: usize) -> Self {
		let mut info = broadcast::Info::default();
		info.pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
		let pool = info.pool.clone();
		let broadcast = broadcast::Producer::new(info);
		let payload = Bytes::from_static(&[0; PAYLOAD]);
		let tracks = (0..tracks)
			.map(|i| {
				let track = broadcast.create_track(format!("bench{i}"), None).unwrap();
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
				group.finish().unwrap();
				track
			})
			.collect();
		// The first call starts the pool's clock and runs its first pass.
		let now = Instant::now();
		pool.gc(now);
		Self {
			_broadcast: broadcast,
			_tracks: tracks,
			pool,
			now,
		}
	}
}

/// Steady viewers on one track, with the producer's aggregate poll caught up.
struct Churn {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	_steady: Vec<track::Subscriber>,
	waiter: kio::Waiter,
}

impl Churn {
	/// `peak` more viewers join and leave first, and the aggregate poll sees them go.
	fn new(steady: usize, peak: usize) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", None).unwrap();
		let steady = (0..steady).map(|_| track.subscribe(None)).collect();
		let peak: Vec<_> = (0..peak).map(|_| track.subscribe(None)).collect();
		let waiter = kio::Waiter::noop();
		assert!(track.poll_subscription_changed(&waiter).is_ready());
		drop(peak);
		assert!(track.poll_subscription_changed(&waiter).is_pending());

		Self {
			_broadcast: broadcast,
			track,
			_steady: steady,
			waiter,
		}
	}

	/// Join and leave `churn` times with the steady viewers' preferences, polling the
	/// aggregate after each departure the way a relay forwarding demand upstream does.
	fn run(&mut self, churn: usize) {
		for _ in 0..churn {
			drop(self.track.subscribe(None));
			assert!(self.track.poll_subscription_changed(&self.waiter).is_pending());
		}
	}
}

fn bench_gc(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_gc");
	for tracks in SWEPT {
		// Between passes, which is every origin poll: should stay flat as tracks grow.
		group.throughput(Throughput::Elements(1));
		group.bench_with_input(BenchmarkId::new("idle", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			b.iter(|| black_box(swept.pool.gc(swept.now)));
		});
		// A due pass, stepping one sweep interval per call.
		group.throughput(Throughput::Elements(tracks as u64));
		group.bench_with_input(BenchmarkId::new("sweep", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			let interval = cache::DEFAULT_EXPIRY / 2;
			let mut now = swept.now;
			b.iter(|| {
				now += interval;
				black_box(swept.pool.gc(now))
			});
		});
	}
	group.finish();
}

fn bench_fanout(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_fanout_group");
	for subscribers in FANOUT {
		group.throughput(Throughput::Elements(subscribers as u64));
		group.bench_with_input(
			BenchmarkId::from_parameter(subscribers),
			&subscribers,
			|b, &subscribers| {
				let mut fanout = Fanout::new(subscribers);
				b.iter(|| fanout.cycle());
			},
		);
	}
	group.finish();
}

/// Write `iterations` single-frame groups across independent tracks sharing one pool.
fn parallel_write(pool: &cache::Pool, writers: usize, iterations: u64) -> Duration {
	let barrier = Arc::new(Barrier::new(writers + 1));
	std::thread::scope(|scope| {
		let handles: Vec<_> = (0..writers)
			.map(|writer| {
				let pool = pool.clone();
				let barrier = barrier.clone();
				let iterations = iterations / writers as u64 + u64::from((writer as u64) < iterations % writers as u64);
				scope.spawn(move || {
					let mut info = broadcast::Info::default();
					info.pool = pool;
					let broadcast = broadcast::Producer::new(info);
					let track = broadcast.create_track("bench", None).unwrap();
					let payload = Bytes::from_static(&[0; PAYLOAD]);
					// Arrive once setup is done, then park until the main thread has stamped the clock.
					barrier.wait();
					barrier.wait();
					for _ in 0..iterations {
						let mut group = track.append_group().unwrap();
						group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
						group.finish().unwrap();
					}
				})
			})
			.collect();

		// The first wait returns once every writer has built its producer, keeping setup out
		// of the interval. The second releases them, so no write lands before the stamp.
		barrier.wait();
		let start = Instant::now();
		barrier.wait();
		for handle in handles {
			handle.join().unwrap();
		}
		start.elapsed()
	})
}

fn bench_parallel_write(c: &mut Criterion) {
	let config = cache::Config::default()
		.with_capacity(CACHE_CAPACITY)
		.with_expiry(cache::DEFAULT_EXPIRY);
	let pool = cache::Pool::new(config);
	let mut group = c.benchmark_group("track_parallel_write");
	// No throughput: one iteration is one frame write, wherever it landed, so the
	// per-iteration time is already the number to compare across writer counts.
	for writers in WRITERS {
		group.bench_with_input(BenchmarkId::from_parameter(writers), &writers, |b, &writers| {
			b.iter_custom(|iterations| parallel_write(&pool, writers, iterations));
		});
	}
	group.finish();
}

fn bench_aborted_scan(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_aborted_scan");
	for aborted in ABORTED {
		group.throughput(Throughput::Elements(aborted as u64));
		group.bench_with_input(BenchmarkId::from_parameter(aborted), &aborted, |b, &aborted| {
			let scan = AbortedScan::new(aborted);
			b.iter(|| scan.scan());
		});
	}
	group.finish();
}

fn bench_subscriber_churn(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_subscriber_churn");
	for steady in STEADY {
		for churn in CHURN {
			group.throughput(Throughput::Elements(churn as u64));
			group.bench_with_input(
				BenchmarkId::new(format!("steady_{steady}"), churn),
				&churn,
				|b, &churn| {
					b.iter_batched_ref(
						|| Churn::new(steady, 0),
						|setup| setup.run(churn),
						BatchSize::SmallInput,
					);
				},
			);
		}
	}
	group.finish();
}

fn bench_subscriber_churn_after_peak(c: &mut Criterion) {
	const CHURN: usize = 512;
	let mut group = c.benchmark_group("track_subscriber_churn_after_peak");
	group.throughput(Throughput::Elements(CHURN as u64));
	for peak in PEAK {
		group.bench_with_input(BenchmarkId::from_parameter(peak), &peak, |b, &peak| {
			b.iter_batched_ref(|| Churn::new(1, peak), |setup| setup.run(CHURN), BatchSize::SmallInput);
		});
	}
	group.finish();
}

fn bench_subscriber_join(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_subscriber_join");
	for join in JOIN {
		group.throughput(Throughput::Elements(join as u64));
		group.bench_with_input(BenchmarkId::from_parameter(join), &join, |b, &join| {
			b.iter_batched(
				|| {
					let broadcast = broadcast::Info::default().produce();
					let track = broadcast.create_track("bench", None).unwrap();
					(broadcast, track)
				},
				|(broadcast, track)| {
					let viewers: Vec<_> = (0..join).map(|_| track.subscribe(None)).collect();
					(broadcast, track, viewers)
				},
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

/// Parked reads re-judge their reach, including the successor's abort waiter.
/// Sweep readers and cached groups so added per-reader scans show up as a slope.
fn bench_parked_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_parked_read");
	for cached in [8, 64, 512] {
		for readers in [1, 8, 64] {
			let broadcast = broadcast::Info::default().produce();
			let track = broadcast.create_track("bench", None).unwrap();
			let mut head = track.append_group().unwrap();
			head.write_frame(Timestamp::ZERO, Bytes::from_static(b"head")).unwrap();
			for _ in 1..cached {
				let mut next = track.append_group().unwrap();
				next.write_frame(Timestamp::ZERO, Bytes::from_static(b"next")).unwrap();
				next.finish().unwrap();
			}
			let waiters: Vec<_> = (0..readers).map(|_| kio::Waiter::noop()).collect();
			let mut held: Vec<_> = waiters
				.iter()
				.map(|waiter| {
					let mut sub =
						track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(1)));
					let Poll::Ready(Ok(Some(mut head))) = sub.poll_recv_group(waiter) else {
						panic!("head is cached");
					};
					assert!(matches!(head.poll_read_frame(waiter), Poll::Ready(Ok(Some(_)))));
					head
				})
				.collect();
			group.throughput(Throughput::Elements(readers as u64));
			group.bench_function(BenchmarkId::new(format!("cached_{cached}"), readers), |b| {
				b.iter(|| {
					for (head, waiter) in held.iter_mut().zip(&waiters) {
						assert!(black_box(head.poll_read_frame(waiter)).is_pending());
					}
				});
			});
		}
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_fanout,
	bench_parallel_write,
	bench_aborted_scan,
	bench_gc,
	bench_subscriber_churn,
	bench_subscriber_churn_after_peak,
	bench_subscriber_join,
	bench_parked_read
);
criterion_main!(benches);
