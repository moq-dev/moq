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
//! `track_parked_read` appends past a backlog of reads parked on their drift budget:
//! each append must wake only the reads it can expire, so waking the backlog shows up
//! as a slope over it.
//!
//! `track_sparse_read` hands out groups from a feed with every other sequence missing,
//! holding a fixed number of gaps the reader still remembers: each hand-out must cost the
//! same however many gaps it holds, so a walk over them shows up as a slope.
//!
//! `track_gap_flush` forgets every remembered gap in one hand-out, each below an aborted
//! group still cached: the successor they share must be found once, not once per gap.
//!
//! `track_wall` is the wall-clock half of staleness, swept over tracks sharing one driven
//! pool and the groups each caches: a collection pass with a parked read's deadline armed on
//! every track (`idle`, none due, and `due`, every one due at once), and re-polling one of
//! those parked reads (`repoll`). A cost that grows with the table instead of the touched
//! path shows up as a slope.
//!
//! `track_empty_run` drains a run of groups that finished without a frame across readers,
//! with the groups after it past the budget, within it, or the whole run retired by the
//! wall clock: each empty group is handed out like any other, so a walk over the rest of
//! the run per group shows up as a slope over it.
//!
//! `track_fetch_expiry` polls budgeted fetched readers over independent fanout and
//! retained-history axes, so expiry work that scans either table appears as a slope.
//!
//! Run with `cargo bench -p moq-net --bench track`.

// Custom timings measure the real time the work took, not the ambient clock.
#![allow(clippy::disallowed_methods)]

use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::task::Poll;
use std::time::{Duration, Instant as WallInstant};

use moq_net::time::Instant;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Error, Timestamp, broadcast, cache, track};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Fanout sizes spanning a direct viewer, a small room, and a large room.
const FANOUT: [usize; 4] = [1, 8, 64, 512];

/// Aborted-prefix sizes spanning a cache hit through a long stale scan.
const ABORTED: [usize; 4] = [1, 8, 64, 512];

/// Small shared payload so the benchmark measures model overhead, not allocation.
const PAYLOAD: usize = 64;

/// Small enough to reach steady-state eviction during Criterion warm-up.
const CACHE_CAPACITY: u64 = 64 * 1024;

/// Steady viewers that stay subscribed while others churn.
const STEADY: [usize; 4] = [1, 100, 1000, 10_000];

/// Viewers that join and leave, one at a time, during one measured run.
const CHURN: [usize; 3] = [1, 8, 64];

/// Viewers that joined and left before the churn, leaving the list room for that many.
const PEAK: [usize; 3] = [0, 1024, 16384];

/// Concurrent publishers sharing one relay-style cache pool.
const WRITERS: [usize; 4] = [1, 2, 4, 8];

/// Tracks sharing one expiring pool: the table a due collection pass walks.
const SWEPT: [usize; 4] = [1, 64, 1_024, 16_384];

/// Tracks sharing one driven pool, for the wall-clock deadlines.
const AGED_TRACKS: [usize; 4] = [1, 64, 1_024, 10_000];

/// Groups cached per track behind its parked read.
const AGED_GROUPS: [usize; 3] = [1, 32, 1_024];

/// The budget a parked read's wall deadline is armed for, past any setup on the real clock.
const AGED_BUDGET: Duration = Duration::from_secs(3600);

/// Fetched readers and retained groups sweep both sides of the expiry lookup.
const FETCH_HISTORY: [usize; 3] = [1, 32, 512];

struct FetchReaders {
	_broadcast: broadcast::Producer,
	_open: moq_net::group::Producer,
	readers: Vec<moq_net::group::Consumer>,
	waiters: Vec<kio::Waiter>,
}

impl FetchReaders {
	fn new(readers: usize, history: usize) -> Self {
		let broadcast = broadcast::Producer::new(broadcast::Info::default());
		let track = broadcast.create_track("bench", timed()).unwrap();
		let mut open = track.append_group().unwrap();
		open.write_frame(Timestamp::ZERO, Bytes::from_static(b"f")).unwrap();
		for sequence in 1..=history {
			let mut group = track.append_group().unwrap();
			group
				.write_frame(
					Timestamp::from_millis(sequence as u64).unwrap(),
					Bytes::from_static(b"f"),
				)
				.unwrap();
			group.finish().unwrap();
		}

		let consumer = track.consume();
		let options = moq_net::group::Fetch::default().with_max_delay(AGED_BUDGET);
		let waiter = kio::Waiter::noop();
		let mut fetched = Vec::with_capacity(readers);
		for _ in 0..readers {
			let mut pending = consumer.fetch_group(0, options.clone());
			let Poll::Ready(Ok(mut group)) = kio::Task::poll(&mut *pending, &waiter) else {
				panic!("cached fetch must resolve");
			};
			assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
			assert!(group.poll_read_frame(&waiter).is_pending());
			fetched.push(group);
		}
		Self {
			_broadcast: broadcast,
			_open: open,
			readers: fetched,
			waiters: (0..readers).map(|_| kio::Waiter::noop()).collect(),
		}
	}

	fn poll(&mut self) {
		for (reader, waiter) in self.readers.iter_mut().zip(&self.waiters) {
			assert!(reader.poll_read_frame(waiter).is_pending());
		}
	}
}

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
		let track = broadcast.create_track("bench", timed()).unwrap();
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
				let track = broadcast.create_track(format!("bench{i}"), timed()).unwrap();
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
				group.finish().unwrap();
				track
			})
			.collect();
		// The first call starts the pool's clock and runs its first pass.
		let now = Instant::now();
		pool.gc(now, &kio::Waiter::noop());
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

/// Tracks sharing one driven pool, each caching `groups` groups behind a read parked on its
/// oldest, which the group after it superseded: one wall-clock deadline per track.
struct Aged {
	_broadcast: broadcast::Producer,
	_tracks: Vec<(track::Producer, track::Subscriber, moq_net::group::Producer)>,
	reads: Vec<(moq_net::group::Consumer, kio::Waiter)>,
	pool: cache::Pool,
	now: Instant,
}

impl Aged {
	fn new(tracks: usize, groups: usize) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let pool = broadcast.info().pool.clone();
		// The driver's first collection, which arms wall-clock deadlines.
		let now = Instant::now();
		pool.gc(now, &kio::Waiter::noop());
		let mut reads = Vec::with_capacity(tracks);
		let tracks = (0..tracks)
			.map(|i| {
				let track = broadcast.create_track(format!("bench{i}"), None).unwrap();
				let mut sub = track.subscribe(track::Subscription::default().with_max_delay(AGED_BUDGET));
				let mut held = track.append_group().unwrap();
				held.write_frame(None, Bytes::from_static(b"x")).unwrap();
				for _ in 1..groups.max(2) {
					let mut group = track.append_group().unwrap();
					group.write_frame(None, Bytes::from_static(b"x")).unwrap();
					group.finish().unwrap();
				}
				let waiter = kio::Waiter::noop();
				let Poll::Ready(Ok(Some(mut read))) = sub.poll_recv_group(&waiter) else {
					panic!("the held group is cached");
				};
				assert!(matches!(read.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
				assert!(read.poll_read_frame(&waiter).is_pending());
				reads.push((read, waiter));
				(track, sub, held)
			})
			.collect();
		Self {
			_broadcast: broadcast,
			_tracks: tracks,
			reads,
			pool,
			now,
		}
	}
}

fn bench_wall(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_wall");
	for tracks in AGED_TRACKS {
		for groups in AGED_GROUPS {
			// Skip the largest table: setup alone would dominate the run.
			if tracks * groups > 1_000_000 {
				continue;
			}
			let id = format!("{tracks}x{groups}");
			// A collection with every deadline armed and none due: every origin poll.
			group.throughput(Throughput::Elements(1));
			group.bench_function(BenchmarkId::new("idle", &id), |b| {
				let aged = Aged::new(tracks, groups);
				b.iter(|| black_box(aged.pool.gc(aged.now, &kio::Waiter::noop())));
			});
			// Re-polling one parked read re-arms its deadline.
			group.bench_function(BenchmarkId::new("repoll", &id), |b| {
				let mut aged = Aged::new(tracks, groups);
				b.iter(|| {
					let (read, waiter) = &mut aged.reads[0];
					assert!(black_box(read.poll_read_frame(waiter)).is_pending());
				});
			});
			// The collection that releases every parked read at once. Its cost is per track
			// released, so the cached groups behind each read are not swept.
			if groups != 1 {
				continue;
			}
			group.throughput(Throughput::Elements(tracks as u64));
			group.bench_function(BenchmarkId::new("due", &id), |b| {
				b.iter_batched(
					|| Aged::new(tracks, groups),
					|aged| {
						// Past every deadline, which setup armed after `now`.
						black_box(aged.pool.gc(aged.now + 2 * AGED_BUDGET, &kio::Waiter::noop()));
						aged
					},
					BatchSize::PerIteration,
				);
			});
		}
	}
	group.finish();
}

fn bench_gc(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_gc");
	for tracks in SWEPT {
		// Between passes, which is every origin poll: should stay flat as tracks grow.
		group.throughput(Throughput::Elements(1));
		group.bench_with_input(BenchmarkId::new("idle", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			b.iter(|| black_box(swept.pool.gc(swept.now, &kio::Waiter::noop())));
		});
		// A due pass, stepping one sweep interval per call.
		group.throughput(Throughput::Elements(tracks as u64));
		group.bench_with_input(BenchmarkId::new("sweep", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			let interval = cache::DEFAULT_EXPIRY / 2;
			let mut now = swept.now;
			b.iter(|| {
				now += interval;
				black_box(swept.pool.gc(now, &kio::Waiter::noop()))
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

fn bench_fetch_expiry(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_fetch_expiry");
	for history in FETCH_HISTORY {
		for readers in FANOUT {
			group.throughput(Throughput::Elements(readers as u64));
			group.bench_with_input(
				BenchmarkId::new(format!("history_{history}"), readers),
				&(readers, history),
				|b, &(readers, history)| {
					let mut fetched = FetchReaders::new(readers, history);
					b.iter(|| fetched.poll());
				},
			);
		}
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
					let track = broadcast.create_track("bench", timed()).unwrap();
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
		let start = WallInstant::now();
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
					let mut setup = Churn::new(steady, 0);
					b.iter(|| setup.run(churn));
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
			let mut setup = Churn::new(1, peak);
			b.iter(|| setup.run(CHURN));
		});
	}
	group.finish();
}

/// One join and its aggregate poll, excluding the departure between iterations.
fn bench_subscriber_join(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_subscriber_join");
	group.throughput(Throughput::Elements(1));
	for steady in STEADY {
		group.bench_function(BenchmarkId::from_parameter(steady), |b| {
			let mut setup = Churn::new(steady, 0);
			b.iter_custom(|iterations| {
				let mut elapsed = Duration::ZERO;
				for _ in 0..iterations {
					let start = Instant::now();
					let viewer = setup.track.subscribe(None);
					assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_pending());
					elapsed += start.elapsed();
					drop(viewer);
					assert!(setup.track.poll_subscription_changed(&setup.waiter).is_pending());
				}
				elapsed
			});
		});
	}
	group.finish();
}

/// Preference changes and an extreme holder leaving, over the already held audience.
///
/// Each held reader asks for distinct values, so the aggregate's tables grow with the audience.
fn bench_subscriber_preferences(c: &mut Criterion) {
	let ordinary = track::Subscription::default()
		.with_start(track::Position::group(100))
		.with_end(track::Position::group(200))
		.with_order(track::Order::Ascending);
	let held = |i: usize| {
		let i = i as u64;
		track::Subscription::default()
			.with_priority((i % 255) as u8)
			.with_max_delay(Duration::from_micros(i))
			.with_start(track::Position::group(100 + i))
			.with_end(track::Position::group(200 + i))
			.with_order(track::Order::Ascending)
	};
	let extreme = track::Subscription::default()
		.with_priority(255)
		.with_max_delay(Duration::from_secs(10));
	let mut group = c.benchmark_group("track_subscriber_preferences");
	for steady in STEADY {
		for touched in CHURN {
			group.throughput(Throughput::Elements(2 * touched as u64));
			group.bench_function(BenchmarkId::new(format!("update/{steady}"), touched), |b| {
				let mut setup = Churn::new(steady, 0);
				for (i, subscriber) in setup._steady.iter_mut().enumerate() {
					subscriber.update(held(i)).unwrap();
				}
				let mut viewer = setup.track.subscribe(ordinary.clone());
				assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
				b.iter(|| {
					for _ in 0..touched {
						viewer.update(extreme.clone()).unwrap();
						assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
						viewer.update(ordinary.clone()).unwrap();
						assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
					}
				});
			});
		}
		group.throughput(Throughput::Elements(1));
		group.bench_function(BenchmarkId::new("extreme_leave", steady), |b| {
			let mut setup = Churn::new(steady, 0);
			for (i, subscriber) in setup._steady.iter_mut().enumerate() {
				subscriber.update(held(i)).unwrap();
			}
			assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
			b.iter_custom(|iterations| {
				let mut elapsed = Duration::ZERO;
				for _ in 0..iterations {
					let viewer = setup.track.subscribe(extreme.clone());
					assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
					let start = Instant::now();
					drop(viewer);
					assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
					elapsed += start.elapsed();
				}
				elapsed
			});
		});
	}
	group.finish();
}

/// Late joins scan a bounded datagram ring; sweep both the backlog and the joining audience.
fn bench_datagram_subscriber_join(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_datagram_subscriber_join");
	for buffered in [0, 1, 8, 64] {
		for join in [1, 8, 64] {
			group.throughput(Throughput::Elements(join as u64));
			group.bench_function(BenchmarkId::new(format!("{buffered}d"), join), |b| {
				b.iter_batched(
					|| {
						let broadcast = broadcast::Info::default().produce();
						let mut track = broadcast.create_track("bench", timed()).unwrap();
						for millis in 0..buffered {
							track
								.append_datagram(Timestamp::from_millis(millis).unwrap(), b"x".as_slice())
								.unwrap();
						}
						(broadcast, track)
					},
					|(broadcast, track)| {
						let preferences = track::Subscription::default().with_max_delay(Duration::from_millis(5));
						let viewers: Vec<_> = (0..join).map(|_| track.subscribe(preferences.clone())).collect();
						(broadcast, track, viewers)
					},
					BatchSize::SmallInput,
				);
			});
		}
	}
	group.finish();
}

/// Queues a parked read as woken, so the bench visits only what an append wakes.
struct Woken {
	read: usize,
	queue: Arc<std::sync::Mutex<Vec<usize>>>,
}

impl std::task::Wake for Woken {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.queue.lock().unwrap().push(self.read);
	}
}

/// One append past a backlog of parked reads, one per group like a publisher's parked
/// group streams, re-polling whatever it wakes. Swept over the backlog so a per-append
/// cost that grows with the parked reads shows up as a slope.
fn bench_parked_read(c: &mut Criterion) {
	const MICROS_PER_GROUP: u64 = 2500;
	let mut group = c.benchmark_group("track_parked_read");
	group.throughput(Throughput::Elements(1));
	for parked in [8, 64, 512] {
		group.bench_function(BenchmarkId::from_parameter(parked), |b| {
			b.iter_custom(|iterations| {
				let broadcast = broadcast::Info::default().produce();
				let track = broadcast.create_track("bench", timed()).unwrap();
				// Cover setup and every measured append so the same backlog stays parked,
				// even when Criterion requests more iterations during warm-up.
				let max_delay = Duration::from_micros(
					iterations
						.checked_add(parked as u64)
						.unwrap()
						.checked_mul(MICROS_PER_GROUP)
						.unwrap(),
				);
				let mut sub = track.subscribe(track::Subscription::default().with_max_delay(max_delay));
				let mut micros = 0;
				let mut open = Vec::with_capacity(parked);
				let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
				let mut reads: Vec<_> = (0..parked)
					.map(|read| {
						let mut group = track.append_group().unwrap();
						group
							.write_frame(Timestamp::from_micros(micros).unwrap(), Bytes::from_static(b"x"))
							.unwrap();
						micros += MICROS_PER_GROUP;
						// Left open, so its read parks instead of ending.
						open.push(group);

						let woken = Woken {
							read,
							queue: queue.clone(),
						};
						let waiter = kio::Waiter::new(std::task::Waker::from(Arc::new(woken)));
						let Poll::Ready(Ok(Some(mut read))) = sub.poll_recv_group(&waiter) else {
							panic!("the group is cached");
						};
						assert!(matches!(read.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
						assert!(read.poll_read_frame(&waiter).is_pending());
						(waiter, read)
					})
					.collect();

				let mut woken = Vec::new();
				let mut repoll = |reads: &mut Vec<(kio::Waiter, moq_net::group::Consumer)>| {
					std::mem::swap(&mut woken, &mut *queue.lock().unwrap());
					for read in woken.drain(..) {
						let (waiter, read) = &mut reads[read];
						assert!(black_box(read.poll_read_frame(waiter)).is_pending());
					}
				};
				// Each append above woke the reads before it: park them again.
				repoll(&mut reads);

				let start = WallInstant::now();
				for _ in 0..iterations {
					let mut next = track.append_group().unwrap();
					next.write_frame(Timestamp::from_micros(micros).unwrap(), Bytes::from_static(b"x"))
						.unwrap();
					next.finish().unwrap();
					micros += MICROS_PER_GROUP;
					repoll(&mut reads);
				}
				start.elapsed()
			});
		});
	}
	group.finish();
}

/// Hand-outs from a sparse feed holding `gaps` remembered gaps, on an untimed track and on
/// a timed one whose budget never writes a gap off. Each iteration hands out the next group,
/// opening a gap at the top, and the lowest missing one, closing the gap at the bottom, so
/// the count stays fixed. Swept over the gaps so a per-hand-out cost that grows with them
/// shows up as a slope.
fn bench_sparse_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_sparse_read");
	group.throughput(Throughput::Elements(2));
	for (name, info) in [("untimed", track::Info::default()), ("timed", timed())] {
		for gaps in [16u64, 256, 4096] {
			group.bench_function(BenchmarkId::new(name, gaps), |b| {
				b.iter_custom(|iterations| {
					let broadcast = broadcast::Info::default().produce();
					let track = broadcast.create_track("bench", info.clone()).unwrap();
					let mut sub =
						track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
					// Microseconds, so no run lasts long enough to cross the budget.
					let timestamp = |sequence: u64| info.timescale.map(|_| Timestamp::from_micros(sequence).unwrap());
					let mut hand_out = |sequence: u64| {
						let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
						group
							.write_frame(timestamp(sequence), Bytes::from_static(b"x"))
							.unwrap();
						group.finish().unwrap();
						let Poll::Ready(Ok(Some(read))) = sub.poll_recv_group(&kio::Waiter::noop()) else {
							panic!("the group is cached");
						};
						black_box(read);
					};
					// Every odd sequence is missing until the bottom gap closes.
					for gap in 0..gaps {
						hand_out(gap * 2);
					}
					let start = WallInstant::now();
					for iteration in 0..iterations {
						hand_out((gaps + iteration) * 2);
						hand_out(iteration * 2 + 1);
					}
					start.elapsed()
				});
			});
		}
	}
	group.finish();
}

/// A reader remembering `gaps` gaps, each below a handed-out group since aborted but still
/// cached, then a stamped survivor after them all.
struct GapFlush {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	sub: track::Subscriber,
	gaps: u64,
}

impl GapFlush {
	fn new(gaps: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let mut sub = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let mut hand_out = |sequence: u64, millis: u64| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(millis).unwrap(), Bytes::from_static(b"x"))
				.unwrap();
			let Poll::Ready(Ok(Some(read))) = sub.poll_recv_group(&kio::Waiter::noop()) else {
				panic!("the group is cached");
			};
			black_box(read);
			group
		};
		// Every odd sequence is missing.
		let handed: Vec<_> = (0..gaps).map(|gap| hand_out(gap * 2, gap)).collect();
		hand_out(gaps * 2, gaps).finish().unwrap();
		for group in handed {
			group.abort(Error::Cancel).unwrap();
		}
		Self {
			_broadcast: broadcast,
			track,
			sub,
			gaps,
		}
	}

	/// Hand out a group two hours on, which puts every remembered gap past the budget.
	fn flush(&mut self) {
		let mut group = self
			.track
			.create_group(moq_net::group::Info {
				sequence: self.gaps * 2 + 1,
			})
			.unwrap();
		group
			.write_frame(Timestamp::from_millis(7_200_000).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		let Poll::Ready(Ok(Some(read))) = self.sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("the group is cached");
		};
		black_box(read);
	}
}

/// One hand-out forgetting every remembered gap at once, each below a group that was
/// aborted but is still cached, swept over the gaps. The groups judged share one
/// successor past the aborted run, so finding it once per gap shows up as a slope.
fn bench_gap_flush(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_gap_flush");
	group.sample_size(10);
	for gaps in [64u64, 512, 4096] {
		group.throughput(Throughput::Elements(gaps));
		group.bench_function(BenchmarkId::from_parameter(gaps), |b| {
			b.iter_batched_ref(|| GapFlush::new(gaps), GapFlush::flush, BatchSize::PerIteration);
		});
	}
	group.finish();
}

/// How the groups after an [`EmptyRun`] judge it.
#[derive(Clone, Copy)]
enum Run {
	/// Two groups an hour apart in media time follow it, writing off its last group.
	Stale,
	/// One group follows it, keeping all of it within the budget.
	Fresh,
	/// A zero budget: the wall clock already retired every superseded group.
	Retired,
}

/// Readers on one track holding group 0, a run of groups that finished without a frame,
/// and the groups after it.
struct EmptyRun {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	readers: Vec<track::Subscriber>,
	/// Groups each reader should be handed.
	handed: usize,
}

impl EmptyRun {
	/// Media-time budgets span an hour, so setup on the real clock never ages a group out on
	/// the wall clock.
	fn new(run: u64, readers: usize, mode: Run) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let append = |millis: Option<u64>| {
			let mut group = track.append_group().unwrap();
			if let Some(millis) = millis {
				group
					.write_frame(Timestamp::from_millis(millis).unwrap(), Bytes::from_static(b"x"))
					.unwrap();
			}
			group.finish().unwrap();
		};
		append(Some(0));
		for _ in 0..run {
			append(None);
		}
		const HOUR: u64 = 3_600_000;
		let after: &[u64] = match mode {
			Run::Stale => &[2 * HOUR, 4 * HOUR],
			Run::Fresh | Run::Retired => &[5],
		};
		for millis in after {
			append(Some(*millis));
		}
		let (budget, handed) = match mode {
			// The run's last group is written off, but two groups follow it.
			Run::Stale => (Duration::from_millis(HOUR), run as usize + 2),
			Run::Fresh => (Duration::from_millis(HOUR), run as usize + 2),
			Run::Retired => (Duration::ZERO, 1),
		};
		let budget = track::Subscription::default().with_max_delay(budget);
		Self {
			readers: (0..readers).map(|_| track.subscribe(budget.clone())).collect(),
			_broadcast: broadcast,
			_track: track,
			handed,
		}
	}

	fn drain(&mut self) {
		for reader in &mut self.readers {
			let mut handed = 0;
			while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&kio::Waiter::noop()) {
				black_box(group);
				handed += 1;
			}
			assert_eq!(handed, self.handed);
		}
	}
}

/// Readers draining a run of groups that finished without a frame, swept over the run and
/// the readers, with the groups after it past the budget (`stale`), within it (`fresh`), or
/// the run retired by the wall clock (`retired`).
/// A reader's cost must stay linear in the run, so a walk over the rest of the run per
/// group shows up as a slope over it.
fn bench_empty_run(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_empty_run");
	group.sample_size(10);
	for (name, mode) in [("stale", Run::Stale), ("fresh", Run::Fresh), ("retired", Run::Retired)] {
		for readers in [1usize, 8, 64] {
			for run in [64u64, 512, 4096] {
				group.throughput(Throughput::Elements(run * readers as u64));
				group.bench_function(BenchmarkId::new(format!("{name}/{readers}"), run), |b| {
					b.iter_batched_ref(
						|| EmptyRun::new(run, readers, mode),
						EmptyRun::drain,
						BatchSize::PerIteration,
					);
				});
			}
		}
	}
	group.finish();
}

/// Abort one non-prefix parked group, swept over backlog and capped subscribers.
fn bench_parked_abort(c: &mut Criterion) {
	let mut bench = c.benchmark_group("track_parked_abort");
	bench.sample_size(10);
	for readers in [1, 8, 64] {
		for retained in [1u64, 64, 1024] {
			bench.bench_function(BenchmarkId::new(readers.to_string(), retained), |b| {
				b.iter_batched_ref(
					|| {
						let broadcast = broadcast::Producer::new(Default::default());
						let track = broadcast.create_track("parked", timed()).unwrap();
						let mut subscribers: Vec<_> = (0..readers)
							.map(|_| {
								let mut subscriber =
									track.subscribe(track::Subscription::default().with_max_delay(AGED_BUDGET));
								subscriber.set_groups(..0);
								subscriber
							})
							.collect();
						let groups: Vec<_> = (0..retained)
							.map(|sequence| {
								let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
								group.write_frame(Timestamp::ZERO, b"f".as_ref()).unwrap();
								group.clone().finish().unwrap();
								group
							})
							.collect();
						for subscriber in &mut subscribers {
							assert!(subscriber.poll_recv_group(&kio::Waiter::noop()).is_pending());
						}
						(broadcast, track, subscribers, groups)
					},
					|(_, _, subscribers, groups)| {
						groups[retained as usize / 2].clone().abort(Error::Old).unwrap();
						for subscriber in subscribers {
							let res = subscriber.poll_recv_group(&kio::Waiter::noop());
							assert!(res.is_pending());
						}
					},
					BatchSize::PerIteration,
				);
			});
		}
	}
	bench.finish();
}

criterion_group!(
	benches,
	bench_parked_abort,
	bench_fanout,
	bench_fetch_expiry,
	bench_parallel_write,
	bench_aborted_scan,
	bench_gc,
	bench_subscriber_churn,
	bench_subscriber_churn_after_peak,
	bench_subscriber_join,
	bench_subscriber_preferences,
	bench_datagram_subscriber_join,
	bench_parked_read,
	bench_sparse_read,
	bench_gap_flush,
	bench_wall,
	bench_empty_run
);
criterion_main!(benches);
