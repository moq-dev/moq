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
//! `track_empty_run` drains a run of groups that finished without a frame across readers,
//! with the groups after it past the budget or within it: each empty group is handed out
//! like any other, so a walk over the rest of the run per group shows up as a slope over it.
//!
//! `track_fanout_frame` writes frames into an open group while every subscriber is parked
//! on the next one, re-polling whichever reader the write woke: a frame wakes no track
//! reader, so waking them shows up as a slope over the subscribers.
//!
//! `track_range_demand` moves one reader between ranges nobody else holds, over readers
//! holding a given number of distinct overlapping ranges: both axes must stay flat, since
//! a change touches only the moved range, never the rest of the audience's.
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
use moq_net::{Error, Timestamp, broadcast, cache, group, track};

/// A millisecond track: the frames written here carry timestamps.
///
/// Unbounded publisher retention, so a subscriber's budget is never clamped below what it asks.
fn timed() -> track::Info {
	track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(Duration::MAX)
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

/// Distinct ranges the steady readers hold between them, capped at the readers.
const DISTINCT: [usize; 4] = [1, 100, 1000, 10_000];

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
		// Also advance supplied time, so reusing an identical instant cannot hide clock work.
		group.bench_with_input(BenchmarkId::new("advancing", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			let mut now = swept.now;
			b.iter(|| {
				now += Duration::from_nanos(1);
				black_box(swept.pool.gc(now))
			});
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

/// Frames written per group in `track_fanout_frame`, after which the group rolls and every
/// subscriber takes the next one: enough that the roll's cost per frame stays small.
const FRAMES_PER_GROUP: usize = 256;

/// Queues its reader's index when woken, so only woken readers are re-polled.
struct Fired {
	index: usize,
	queue: Arc<std::sync::Mutex<Vec<usize>>>,
}

impl futures::task::ArcWake for Fired {
	fn wake_by_ref(fired: &Arc<Self>) {
		fired.queue.lock().unwrap().push(fired.index);
	}
}

fn bench_fanout_frame(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_fanout_frame");
	for subscribers in FANOUT {
		group.bench_with_input(
			BenchmarkId::from_parameter(subscribers),
			&subscribers,
			|b, &subscribers| {
				let mut fanout = Fanout::new(subscribers);
				let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
				let waiters: Vec<_> = (0..subscribers)
					.map(|index| {
						let fired = Arc::new(Fired {
							index,
							queue: queue.clone(),
						});
						kio::Waiter::new(futures::task::waker(fired))
					})
					.collect();
				let mut open: Option<moq_net::group::Producer> = None;
				let mut written = FRAMES_PER_GROUP;
				// Only the frame write and the re-polls it causes are timed, not the group roll.
				b.iter_custom(|iterations| {
					let mut elapsed = Duration::ZERO;
					for _ in 0..iterations {
						if written == FRAMES_PER_GROUP {
							if let Some(open) = open.take() {
								open.finish().unwrap();
							}
							open = Some(fanout.track.append_group().unwrap());
							written = 0;
							for (subscriber, waiter) in fanout.subscribers.iter_mut().zip(&waiters) {
								assert!(matches!(subscriber.poll_recv_group(waiter), Poll::Ready(Ok(Some(_)))));
								assert!(subscriber.poll_recv_group(waiter).is_pending());
							}
							queue.lock().unwrap().clear();
						}
						let group = open.as_mut().unwrap();
						let start = Instant::now();
						group.write_frame(Timestamp::ZERO, fanout.payload.clone()).unwrap();
						// A woken reader re-polls, as its task would.
						let woken = std::mem::take(&mut *queue.lock().unwrap());
						for index in woken {
							black_box(fanout.subscribers[index].poll_recv_group(&waiters[index]).is_pending());
						}
						elapsed += start.elapsed();
						written += 1;
					}
					elapsed
				});
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
	let ordinary = track::Subscription::default().with_groups(100..200);
	let held = |i: usize| {
		let i = i as u64;
		track::Subscription::default()
			.with_priority((i % 255) as u8)
			.with_max_age(Duration::from_micros(i))
			.with_groups(100 + i..200 + i)
	};
	let extreme = track::Subscription::default()
		.with_priority(255)
		.with_max_age(Duration::from_secs(10));
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

/// One reader moving between ranges of its own, over readers holding `distinct` ranges.
fn bench_range_demand(c: &mut Criterion) {
	// Far past every held range, so each move adds one run and removes another.
	let away = |offset: u64| {
		let group = (1 << 40) + offset;
		track::Subscription::default().with_groups(group..=group)
	};
	let mut group = c.benchmark_group("track_range_demand");
	for steady in STEADY {
		for distinct in DISTINCT.into_iter().filter(|&distinct| distinct <= steady) {
			group.throughput(Throughput::Elements(2));
			group.bench_function(BenchmarkId::new(format!("{steady}r"), distinct), |b| {
				let mut setup = Churn::new(steady, 0);
				for (i, subscriber) in setup._steady.iter_mut().enumerate() {
					// Overlapping, so the held ranges union into one run.
					let start = (i % distinct) as u64 * 10;
					subscriber
						.update(track::Subscription::default().with_groups(start..start + 1000))
						.unwrap();
				}
				let mut viewer = setup.track.subscribe(away(0));
				assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
				b.iter(|| {
					viewer.update(away(2)).unwrap();
					assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
					viewer.update(away(0)).unwrap();
					assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
				});
			});
		}
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
						let preferences = track::Subscription::default().with_max_age(Duration::from_millis(5));
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
				let mut sub = track.subscribe(track::Subscription::default().with_max_age(max_delay));
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

/// Hand-outs from a sparse feed holding `gaps` remembered gaps, on a timed track whose
/// budget never writes a gap off. Each iteration hands out the next group,
/// opening a gap at the top, and the lowest missing one, closing the gap at the bottom, so
/// the count stays fixed. Swept over the gaps so a per-hand-out cost that grows with them
/// shows up as a slope.
fn bench_sparse_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_sparse_read");
	group.throughput(Throughput::Elements(2));
	for gaps in [16u64, 256, 4096] {
		group.bench_function(BenchmarkId::new("timed", gaps), |b| {
			b.iter_custom(|iterations| {
				let broadcast = broadcast::Info::default().produce();
				let track = broadcast.create_track("bench", timed()).unwrap();
				let mut sub = track.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(3600)));
				// Microseconds, so no run lasts long enough to cross the budget.
				let timestamp = |sequence: u64| Timestamp::from_micros(sequence).unwrap();
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
		let mut sub = track.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(3600)));
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
	for gaps in [16u64, 64, 256, 512, 4096] {
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
			Run::Fresh => &[5],
		};
		for millis in after {
			append(Some(*millis));
		}
		let (budget, handed) = match mode {
			// The run's last group is written off, but two groups follow it.
			Run::Stale => (Duration::from_millis(HOUR), run as usize + 2),
			Run::Fresh => (Duration::from_millis(HOUR), run as usize + 2),
		};
		let budget = track::Subscription::default().with_max_age(budget);
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
/// the readers, with the groups after it past the budget (`stale`) or within it (`fresh`).
/// A reader's cost must stay linear in the run, so a walk over the rest of the run per
/// group shows up as a slope over it.
fn bench_empty_run(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_empty_run");
	group.sample_size(10);
	for (name, mode) in [("stale", Run::Stale), ("fresh", Run::Fresh)] {
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

/// Groups written between reads in `track_evict_fanout`, each evicting one.
const EVICTED: [usize; 3] = [1, 8, 64];

fn bench_evict_fanout(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_evict_fanout");
	for subscribers in FANOUT {
		for evicted in EVICTED {
			group.throughput(Throughput::Elements(evicted as u64));
			group.bench_function(BenchmarkId::new(format!("{subscribers}r"), evicted), |b| {
				let mut fanout = Fanout::new(subscribers);
				// Twice what the pool holds, so every write evicts.
				for _ in 0..2 * CACHE_CAPACITY / PAYLOAD as u64 {
					fanout.cycle();
				}
				// Only the writes and the evictions they cause are timed, not the reads that
				// judge each written group cached again.
				b.iter_custom(|iterations| {
					let mut elapsed = Duration::ZERO;
					for _ in 0..iterations {
						let start = WallInstant::now();
						for _ in 0..evicted {
							let mut group = fanout.track.append_group().unwrap();
							group.write_frame(Timestamp::ZERO, fanout.payload.clone()).unwrap();
							group.finish().unwrap();
						}
						elapsed += start.elapsed();
						for (subscriber, waiter) in fanout.subscribers.iter_mut().zip(&fanout.waiters) {
							while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(waiter) {
								black_box(group);
							}
						}
					}
					elapsed
				});
			});
		}
	}
	group.finish();
}

/// Tracks sharing one bounded pool, each holding `queued` datagrams for a reader that never
/// reads, while one datagram is appended to the next track and read by its fast reader.
///
/// Only `main`'s `plain` payload: `release` has no track compression, so its `zeros` rows
/// are left out.
fn bench_datagram_pressure(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_datagram_pressure");
	group.throughput(Throughput::Elements(1));
	let name = "plain";
	let payload = Bytes::from(vec![0u8; PAYLOAD]);
	for tracks in [1, 16, 64] {
		for queued in [1, 8, 64] {
			let mut info = broadcast::Info::default();
			info.pool = cache::Pool::new(cache::Config::default().with_capacity(CACHE_CAPACITY));
			let pool = info.pool.clone();
			let broadcast = broadcast::Producer::new(info);
			let mut held: Vec<_> = (0..tracks)
				.map(|index| {
					let mut track = broadcast
						.create_track(format!("bench{index}").as_str(), timed())
						.unwrap();
					let slow = track.subscribe(None);
					let fast = track.subscribe(None);
					for _ in 0..queued {
						track.append_datagram(Timestamp::ZERO, payload.clone()).unwrap();
					}
					(track, slow, fast)
				})
				.collect();
			let waiter = kio::Waiter::noop();
			for (_, _, fast) in &mut held {
				while let Poll::Ready(Ok(Some(_))) = fast.poll_recv_datagram(&waiter) {}
			}
			eprintln!(
				"track_datagram_pressure/{name}/{tracks}t/{queued}q: {} bytes retained",
				pool.used()
			);

			let mut next = 0;
			group.bench_function(BenchmarkId::new(format!("{name}/{tracks}t"), queued), |b| {
				b.iter(|| {
					let (track, _, fast) = &mut held[next % tracks];
					next += 1;
					track.append_datagram(Timestamp::ZERO, payload.clone()).unwrap();
					let Poll::Ready(Ok(Some(datagram))) = fast.poll_recv_datagram(&waiter) else {
						panic!("the fast reader takes each datagram");
					};
					black_box(datagram);
				});
			});
			drop((held, broadcast));
		}
	}
	group.finish();
}

/// A timed sparse feed holding `gaps` remembered gaps, as in `track_sparse_read`.
struct Sparse {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	sub: track::Subscriber,
	next: u64,
	missing: u64,
}

impl Sparse {
	fn new(gaps: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let sub = track.subscribe(track::Subscription::default().with_max_age(Duration::from_secs(3600)));
		let mut sparse = Self {
			_broadcast: broadcast,
			track,
			sub,
			next: gaps * 2,
			missing: 1,
		};
		for gap in 0..gaps {
			sparse.hand_out(gap * 2);
		}
		// Fill beyond the cache queue's initial power-of-two capacity outside timing.
		if gaps != 0 {
			sparse.hand_out(sparse.next);
			sparse.hand_out(sparse.missing);
			sparse.next += 2;
			sparse.missing += 2;
		}
		sparse
	}

	fn hand_out(&mut self, sequence: u64) {
		let mut group = self.track.create_group(moq_net::group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::from_micros(sequence).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		group.finish().unwrap();
		let Poll::Ready(Ok(Some(read))) = self.sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("the group is cached");
		};
		black_box(read);
	}
}

/// A read with nothing new on a sparse feed, swept over its remembered gaps. Only `main`'s
/// `timed` rows: `release` reads no untimed sparse feed.
fn bench_sparse_poll(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_sparse_poll");
	for gaps in [16u64, 256, 4096] {
		group.bench_function(BenchmarkId::new("timed", gaps), |b| {
			b.iter_custom(|iterations| {
				let mut sparse = Sparse::new(gaps);
				let waiter = kio::Waiter::noop();
				let start = WallInstant::now();
				for _ in 0..iterations {
					assert!(black_box(sparse.sub.poll_recv_group(&waiter)).is_pending());
				}
				start.elapsed()
			});
		});
	}
	group.finish();
}

/// The budget `track_idle_poll`'s readers hold, long enough that nothing ages out.
const AGED_BUDGET: Duration = Duration::from_secs(3600);

/// Readers parked on one track with nothing new, each having taken the one cached group.
struct Idle {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	_open: group::Producer,
	recv: Vec<track::Subscriber>,
	next: Vec<track::Ordered>,
	waiter: kio::Waiter,
}

impl Idle {
	fn new(readers: usize, ordered: bool) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let mut open = track.append_group().unwrap();
		open.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		let waiter = kio::Waiter::noop();
		let (mut recv, mut next) = (Vec::new(), Vec::new());
		for _ in 0..readers {
			let reader = track.subscribe(track::Subscription::default().with_max_age(AGED_BUDGET));
			if ordered {
				let mut reader = reader.ordered();
				assert!(matches!(reader.poll_next_group(&waiter), Poll::Ready(Ok(Some(_)))));
				next.push(reader);
			} else {
				let mut reader = reader;
				assert!(matches!(reader.poll_recv_group(&waiter), Poll::Ready(Ok(Some(_)))));
				recv.push(reader);
			}
		}
		let mut idle = Self {
			_broadcast: broadcast,
			_track: track,
			_open: open,
			recv,
			next,
			waiter,
		};
		// The first park walks the read path; only the repeats are measured.
		idle.poll();
		idle
	}

	fn poll(&mut self) {
		for reader in &mut self.recv {
			assert!(black_box(reader.poll_recv_group(&self.waiter)).is_pending());
		}
		for reader in &mut self.next {
			assert!(black_box(reader.poll_next_group(&self.waiter)).is_pending());
		}
	}
}

fn bench_idle_poll(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_idle_poll");
	for (name, ordered) in [("recv", false), ("next", true)] {
		for readers in FANOUT {
			group.throughput(Throughput::Elements(readers as u64));
			group.bench_function(BenchmarkId::new(name, readers), |b| {
				let mut idle = Idle::new(readers, ordered);
				b.iter(|| idle.poll());
			});
		}
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_fanout,
	bench_fanout_frame,
	bench_parallel_write,
	bench_aborted_scan,
	bench_gc,
	bench_subscriber_churn,
	bench_subscriber_churn_after_peak,
	bench_subscriber_join,
	bench_range_demand,
	bench_subscriber_preferences,
	bench_datagram_subscriber_join,
	bench_parked_read,
	bench_sparse_read,
	bench_gap_flush,
	bench_empty_run,
	bench_evict_fanout,
	bench_datagram_pressure,
	bench_sparse_poll,
	bench_idle_poll
);
criterion_main!(benches);
