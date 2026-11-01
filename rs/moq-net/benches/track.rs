//! Producer-to-subscriber fanout benchmarks for the track model.
//!
//! The group benchmarks isolate per-frame storage. These benchmarks include the
//! track cache and subscription cursors around that storage: append a complete
//! group, notify every subscriber, and hand the cached group to each cursor.
//!
//! `track_aborted_scan` isolates the other half of delivery: how much a cached
//! prefix of aborted groups costs the scan that has to walk past it.
//!
//! `track_gc` is the cache's clock-driven half: the per-poll [`cache::Pool::poll`]
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
//! `track_sparse_poll` repeats a parked read without changing the feed, so revisiting
//! deferred gaps on an otherwise unchanged poll shows up independently of delivery.
//!
//! `track_sparse_expire` opens one new gap while one old gap expires, so a full expiry
//! rescan on each arrival shows up as a slope over the gaps still within budget.
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
//! `track_evict_fanout` writes groups into a full pool, each evicting one that every reader
//! judged cached, swept over the readers and the groups written between their reads: each
//! eviction leaves the group in every reader's inbox, so its cost grows with the readers,
//! and a cost that grows with the groups written beyond that shows up as a slope.
//!
//! `track_fanout_frame` writes frames into an open group while every subscriber is parked
//! on the next one, re-polling whichever reader the write woke: a frame wakes no track
//! reader, so waking them shows up as a slope over the subscribers.
//!
//! `track_fetch_expiry` polls budgeted fetched readers over independent fanout and
//! retained-history axes, so expiry work that scans either table appears as a slope.
//!
//! `track_fetch_budgets` writes the frame that crosses every budgeted fetch's deadline, over
//! live readers and finished fetches with budgets of their own: deadlines the finished
//! fetches left indexed show up as a slope over them.
//!
//! `track_fetch_park` parks one more budgeted fetch beside live readers on deadlines of their
//! own, so a park that sweeps the whole index shows up as a slope over the readers.
//!
//! `track_range_demand` moves one reader between ranges nobody else holds, over readers
//! holding a given number of distinct overlapping ranges: both axes must stay flat, since
//! a change touches only the moved range, never the rest of the audience's.
//!
//! `track_subscription_ranges` reads the aggregate of subscribers that each ask for a
//! disjoint range, swept over how many ranges and how many groups each spans: reading the
//! union must cost the same per range whatever it spans.
//!
//! `track_range_serving` drains one reader whose own ranges are disjoint, swept over how
//! many ranges and how many cached groups each spans: each group handed out must cost the
//! same, so a walk over the ranges or the groups between them shows up as a slope.
//!
//! `track_range_widening` widens one reader's ranges by a fixed span of cached groups and
//! back, swept over the groups cached and the readers beside it: a reader holds nothing
//! for the groups outside its ranges, so a widening scans only the span it newly covers,
//! and a scan of the cache or the audience shows up as a slope.
//!
//! `track_budget_widening` widens one reader's budget past the cached groups it skipped as
//! stale, each between two it handed out, swept over the readers beside it, the groups
//! cached, and the runs it skipped: a widening costs the runs it re-offers, so a scan of the
//! cache or the audience shows up as a slope.
//!
//! `track_datagram_pressure` appends datagrams across tracks sharing a bounded pool, each
//! holding a backlog for a reader that never reads, with small plain payloads and with
//! compressed zeros that inflate far past their wire size: the pool bills both forms, so the
//! retained bytes stay near capacity and the append cost stays flat over tracks and backlog.
//!
//! `track_time_end` lands groups one at a time behind a sequence that never arrives, each
//! drained by readers whose range ends at a time no group reaches, swept over the groups and
//! the readers: an unsettled time end walks on from where it stopped, so a rescan of the
//! groups behind the gap on every read shows up as a slope over them.
//!
//! `track_fill` has readers miss the same runs below where the feed starts, each asked of the
//! track's handler as a fill, swept over the readers and the runs: readers share each
//! fill, so asking, updating, dropping, and settling must cost each reader the same per
//! run, and a cost that grows with the audience or the runs beyond that shows up as a slope.
//!
//! `track_range_filling` has one reader fill disjoint ranges below where the feed starts,
//! swept over how many ranges and how many groups each spans, the filling counterpart of
//! `track_range_serving`: each group filled must cost the same, so a walk over the ranges or
//! the groups of each fill shows up as a slope.
//!
//! Run with `cargo bench -p moq-net --bench track`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::task::Poll;
use std::time::{Duration, Instant as WallInstant};

use moq_net::time::Instant;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Error, Timestamp, broadcast, cache, group, track};

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

/// Distinct ranges the steady readers hold between them, capped at the readers.
const DISTINCT: [usize; 4] = [1, 100, 1000, 10_000];

/// Disjoint ranges in one aggregate, one per subscriber.
const RANGES: [u64; 4] = [1, 16, 256, 4096];

/// Groups each aggregated range spans.
const SPAN: [u64; 3] = [1, 64, 4096];

/// Disjoint ranges one reader is served, and the cached groups each spans.
const SERVED_RANGES: [u64; 3] = [1, 16, 256];
const SERVED_SPAN: [u64; 3] = [1, 8, 64];

/// Readers beside the one widening, and the groups cached around the span it widens by.
const WIDENING_READERS: [usize; 3] = [1, 64, 1024];
const WIDENING_CACHED: [u64; 3] = [64, 1024, 16_384];

/// Cached groups one widening newly covers.
const WIDENED: u64 = 8;

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
	_track: track::Producer,
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
			_track: track,
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
		pool.poll(now, &kio::Waiter::noop());
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
		pool.poll(now, &kio::Waiter::noop());
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
				let waiter = kio::Waiter::noop();
				b.iter(|| black_box(aged.pool.poll(aged.now, &waiter)));
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
						black_box(aged.pool.poll(aged.now + 2 * AGED_BUDGET, &kio::Waiter::noop()));
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
			let waiter = kio::Waiter::noop();
			b.iter(|| black_box(swept.pool.poll(swept.now, &waiter)));
		});
		// Also advance supplied time, so reusing an identical instant cannot hide clock work.
		group.bench_with_input(BenchmarkId::new("advancing", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			let mut now = swept.now;
			let waiter = kio::Waiter::noop();
			b.iter(|| {
				now += Duration::from_nanos(1);
				black_box(swept.pool.poll(now, &waiter))
			});
		});
		// A due pass, stepping one sweep interval per call.
		group.throughput(Throughput::Elements(tracks as u64));
		group.bench_with_input(BenchmarkId::new("sweep", tracks), &tracks, |b, &tracks| {
			let swept = Swept::new(tracks);
			let interval = cache::DEFAULT_EXPIRY / 2;
			let mut now = swept.now;
			let waiter = kio::Waiter::noop();
			b.iter(|| {
				now += interval;
				black_box(swept.pool.poll(now, &waiter))
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
						let start = Instant::now();
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
							if let Some(mut open) = open.take() {
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

/// A track with groups presenting at 0..4 s and the last still open, `readers` budgeted
/// fetches of group 2 parked on its expiry, and `history` finished fetches of it, each with
/// its own budget, so each indexed a deadline of its own.
struct FetchBudgets {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	consumer: track::Consumer,
	open: moq_net::group::Producer,
	/// The live fetches' waiters, which keep their deadlines registered.
	_readers: Vec<kio::Waiter>,
}

impl FetchBudgets {
	fn new(readers: usize, history: usize) -> Self {
		let broadcast = broadcast::Producer::new(broadcast::Info::default());
		let track = broadcast.create_track("bench", timed()).unwrap();
		for second in 0..4u64 {
			let mut group = track.append_group().unwrap();
			group
				.write_frame(Timestamp::from_secs(second).unwrap(), Bytes::from_static(b"f"))
				.unwrap();
			group.finish().unwrap();
		}
		let mut open = track.append_group().unwrap();
		open.write_frame(Timestamp::from_secs(4).unwrap(), Bytes::from_static(b"f"))
			.unwrap();

		let mut fetched = Self {
			_broadcast: broadcast,
			consumer: track.consume(),
			_track: track,
			open,
			_readers: Vec::with_capacity(readers),
		};
		for i in 0..history {
			drop(fetched.fetch(readers + i));
		}
		fetched._readers = (0..readers).map(|i| fetched.fetch(i)).collect();
		fetched
	}

	/// Fetch group 2 with budget `i`, distinct from every other, returning the waiter that
	/// keeps its deadlines registered.
	fn fetch(&self, i: usize) -> kio::Waiter {
		let waiter = kio::Waiter::noop();
		let budget = Duration::from_millis(1_500) + Duration::from_micros(i as u64);
		let options = moq_net::group::Fetch::default().with_max_delay(budget);
		let mut pending = self.consumer.fetch_group(2, options);
		assert!(matches!(kio::Task::poll(&mut *pending, &waiter), Poll::Ready(Ok(_))));
		waiter
	}
}

/// One more budgeted fetch, with a budget of its own, beside `readers` live ones: parking it
/// must not scan the deadlines the live readers hold, so a sweep per park shows up as a slope.
fn bench_fetch_park(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_fetch_park");
	for readers in [1, 64, 1_024] {
		group.bench_with_input(BenchmarkId::from_parameter(readers), &readers, |b, &readers| {
			let fetched = FetchBudgets::new(readers, 0);
			let mut next = readers;
			b.iter(|| {
				next += 1;
				fetched.fetch(next)
			});
		});
	}
	group.finish();
}

/// The write that crosses every budgeted fetch's deadline, beside live readers and the
/// finished fetches before them: the write must cost what the live readers do, so deadlines
/// left indexed by fetches that already finished show up as a slope over the history.
fn bench_fetch_budgets(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_fetch_budgets");
	for history in [0, 1_024, 16_384] {
		for readers in [1, 64] {
			group.bench_with_input(
				BenchmarkId::new(format!("history_{history}"), readers),
				&(readers, history),
				|b, &(readers, history)| {
					b.iter_batched(
						|| FetchBudgets::new(readers, history),
						|mut fetched| {
							fetched
								.open
								.write_frame(Timestamp::from_secs(10).unwrap(), Bytes::from_static(b"f"))
								.unwrap();
							fetched
						},
						BatchSize::PerIteration,
					);
				},
			);
		}
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
		.with_range(100..200)
		.with_order(track::Order::Ascending);
	let held = |i: usize| {
		let i = i as u64;
		track::Subscription::default()
			.with_priority(((i % 255) as i16 - 128) as i8)
			.with_max_delay(Duration::from_micros(i))
			.with_range(100 + i..200 + i)
			.with_order(track::Order::Ascending)
	};
	let extreme = track::Subscription::default()
		.with_priority(i8::MAX)
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

/// One reader moving between ranges of its own, over readers holding `distinct` ranges.
fn bench_range_demand(c: &mut Criterion) {
	// Far past every held range, so each move adds one run and removes another.
	let away = |offset: u64| {
		let group = (1 << 40) + offset;
		track::Subscription::default().with_range(group..=group)
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
						.update(track::Subscription::default().with_range(start..start + 1000))
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

fn bench_subscription_ranges(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_subscription_ranges");
	for span in SPAN {
		for ranges in RANGES {
			let broadcast = broadcast::Info::default().produce();
			let track = broadcast.create_track("bench", None).unwrap();
			// A gap between neighbors keeps every range distinct in the union.
			let viewers: Vec<_> = (0..ranges)
				.map(|i| {
					let start = i * (span + 1);
					track.subscribe(track::Subscription::default().with_range(start..start + span))
				})
				.collect();
			assert_eq!(track.subscription().unwrap().ranges.len(), ranges as usize);

			group.throughput(Throughput::Elements(ranges));
			group.bench_function(BenchmarkId::new(format!("span_{span}"), ranges), |b| {
				b.iter(|| black_box(track.subscription()));
			});
			drop((viewers, track, broadcast));
		}
	}
	group.finish();
}

fn bench_range_serving(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_range_serving");
	for span in SERVED_SPAN {
		for ranges in SERVED_RANGES {
			let broadcast = broadcast::Info::default().produce();
			let track = broadcast.create_track("bench", None).unwrap();
			// A gap between neighbors keeps every range distinct, and its group cached.
			for _ in 0..ranges * (span + 1) {
				let mut group = track.append_group().unwrap();
				group.write_frame(None, Bytes::from_static(b"x")).unwrap();
				group.finish().unwrap();
			}
			let subscription = track::Subscription::default()
				.with_max_delay(AGED_BUDGET)
				.with_ranges((0..ranges).map(|i| {
					let start = i * (span + 1);
					track::Range::groups(start..start + span)
				}));

			group.throughput(Throughput::Elements(ranges * span));
			group.bench_function(BenchmarkId::new(format!("span_{span}"), ranges), |b| {
				b.iter_batched(
					|| track.subscribe(subscription.clone()),
					|mut reader| {
						let waiter = kio::Waiter::noop();
						let mut served = 0;
						while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
							black_box(group);
							served += 1;
						}
						assert_eq!(served, ranges * span);
						reader
					},
					BatchSize::SmallInput,
				);
			});
			drop((track, broadcast));
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

/// Datagram payloads for `track_datagram_pressure`: a small plain one, and the largest
/// payload of zeros on a compressed track, which inflates far past its DEFLATE form.
const DATAGRAM_PAYLOADS: [(&str, usize, bool); 2] = [("plain", PAYLOAD, false), ("zeros", u16::MAX as usize, true)];

/// Tracks sharing one bounded pool, each holding `queued` datagrams for a reader that never
/// reads, while one datagram is appended to the next track and read by its fast reader.
///
/// The pool bills every buffered datagram, so what the slow readers retain stays near the
/// pool's capacity however many tracks or queued datagrams there are (printed per case), and
/// appending must cost the same: a walk over the other tracks or the backlog shows as a slope.
fn bench_datagram_pressure(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_datagram_pressure");
	group.throughput(Throughput::Elements(1));
	for (name, size, compress) in DATAGRAM_PAYLOADS {
		let payload = Bytes::from(vec![0u8; size]);
		for tracks in [1, 16, 64] {
			for queued in [1, 8, 64] {
				let mut info = broadcast::Info::default();
				info.pool = cache::Pool::new(cache::Config::default().with_capacity(CACHE_CAPACITY));
				let pool = info.pool.clone();
				let broadcast = broadcast::Producer::new(info);
				let mut held: Vec<_> = (0..tracks)
					.map(|index| {
						let mut track = broadcast
							.create_track(format!("bench{index}").as_str(), timed().with_compress(compress))
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
struct Sparse {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	sub: track::Subscriber,
	next: u64,
	missing: u64,
	timed: bool,
}

impl Sparse {
	fn new(info: track::Info, gaps: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", info.clone()).unwrap();
		let sub = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let mut sparse = Self {
			_broadcast: broadcast,
			track,
			sub,
			next: gaps * 2,
			missing: 1,
			timed: info.timescale.is_some(),
		};
		for gap in 0..gaps {
			sparse.hand_out(gap * 2);
		}
		// Fill beyond the cache queue's initial power-of-two capacity outside timing.
		// Its amortized growth is independent of how many ledger gaps remain open.
		if gaps != 0 {
			sparse.advance();
		}
		sparse
	}

	fn hand_out(&mut self, sequence: u64) {
		let timestamp = self.timed.then(|| Timestamp::from_micros(sequence).unwrap());
		let mut group = self.track.create_group(moq_net::group::Info { sequence }).unwrap();
		group.write_frame(timestamp, Bytes::from_static(b"x")).unwrap();
		group.finish().unwrap();
		let Poll::Ready(Ok(Some(read))) = self.sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("the group is cached");
		};
		black_box(read);
	}

	// A profiling boundary excludes filling the initial sparse backlog.
	#[inline(never)]
	fn read_pair(&mut self) {
		self.advance();
	}

	fn advance(&mut self) {
		self.hand_out(self.next);
		self.hand_out(self.missing);
		self.next += 2;
		self.missing += 2;
	}
}

fn bench_sparse_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_sparse_read");
	group.throughput(Throughput::Elements(2));
	for (name, info) in [("untimed", track::Info::default()), ("timed", timed())] {
		for gaps in [16u64, 256, 4096] {
			group.bench_function(BenchmarkId::new(name, gaps), |b| {
				b.iter_custom(|iterations| {
					let mut sparse = Sparse::new(info.clone(), gaps);
					let start = WallInstant::now();
					for _ in 0..iterations {
						sparse.read_pair();
					}
					start.elapsed()
				});
			});
		}
	}
	group.finish();
}

fn bench_sparse_poll(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_sparse_poll");
	for (name, info) in [("untimed", track::Info::default()), ("timed", timed())] {
		for gaps in [16u64, 256, 4096] {
			group.bench_function(BenchmarkId::new(name, gaps), |b| {
				b.iter_custom(|iterations| {
					let mut sparse = Sparse::new(info.clone(), gaps);
					let waiter = kio::Waiter::noop();
					let start = WallInstant::now();
					for _ in 0..iterations {
						assert!(black_box(sparse.sub.poll_recv_group(&waiter)).is_pending());
					}
					start.elapsed()
				});
			});
		}
	}
	group.finish();
}

/// A sparse feed that opens one gap per millisecond, keeping `gaps` within its budget.
struct Expiring {
	sparse: Sparse,
	pool: cache::Pool,
	now: Instant,
}

impl Expiring {
	fn new(gaps: u64) -> Self {
		let mut sparse = Sparse::new(track::Info::default(), 0);
		sparse
			.sub
			.update(track::Subscription::default().with_max_delay(Duration::from_millis(gaps)))
			.unwrap();
		let pool = sparse._broadcast.info().pool.clone();
		// Supply time ahead of the host clock, so only these steps age the gaps even
		// under an instruction profiler. No cache capacity or idle expiry is enabled.
		let mut fixture = Self {
			sparse,
			pool,
			now: Instant::now() + Duration::from_secs(3600),
		};
		for gap in 0..gaps {
			fixture.pool.poll(fixture.now, &kio::Waiter::noop());
			fixture.sparse.hand_out(gap * 2);
			fixture.now += Duration::from_millis(1);
		}
		fixture.sparse.next = gaps * 2;
		// The eviction queue trails arrivals by one entry: warm both capacities.
		fixture.step();
		fixture.step();
		fixture
	}

	// One new arrival expires one old gap; excludes building the initial ledger.
	#[inline(never)]
	fn arrive(&mut self) {
		self.step();
	}

	fn step(&mut self) {
		self.now += Duration::from_millis(1);
		self.pool.poll(self.now, &kio::Waiter::noop());
		self.sparse.hand_out(self.sparse.next);
		self.sparse.next += 2;
	}
}

fn bench_sparse_expire(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_sparse_expire");
	for gaps in [16u64, 256, 4096] {
		group.bench_function(BenchmarkId::from_parameter(gaps), |b| {
			b.iter_custom(|iterations| {
				let mut expiring = Expiring::new(gaps);
				let start = WallInstant::now();
				for _ in 0..iterations {
					expiring.arrive();
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
	// A profiling boundary excludes publishing the groups and their aborts.
	#[inline(never)]
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

/// One reader widening its ranges by [`WIDENED`] cached groups mid-cache and back, beside
/// idle narrow readers. Every widening after the first finds groups already handed out,
/// which the reader skips, so each one costs the same scan of the span.
fn bench_range_widening(c: &mut Criterion) {
	let mut bench = c.benchmark_group("track_range_widening");
	bench.throughput(Throughput::Elements(1));
	for readers in WIDENING_READERS {
		for cached in WIDENING_CACHED {
			bench.bench_function(BenchmarkId::new(format!("{readers}r"), cached), |b| {
				let broadcast = broadcast::Info::default().produce();
				let track = broadcast.create_track("bench", None).unwrap();
				for _ in 0..cached {
					let mut group = track.append_group().unwrap();
					group.write_frame(None, Bytes::from_static(b"x")).unwrap();
					group.finish().unwrap();
				}
				let narrow = track::Subscription::default()
					.with_max_delay(AGED_BUDGET)
					.with_range(0..1);
				let mid = cached / 2;
				let wide = narrow
					.clone()
					.with_ranges([track::Range::groups(0..1), track::Range::groups(mid..mid + WIDENED)]);
				let waiter = kio::Waiter::noop();
				let mut readers: Vec<_> = (0..readers)
					.map(|_| {
						let mut reader = track.subscribe(narrow.clone());
						while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
						reader
					})
					.collect();
				let reader = &mut readers[0];
				b.iter(|| {
					reader.update(wide.clone()).unwrap();
					while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
						black_box(group);
					}
					reader.update(narrow.clone()).unwrap();
					assert!(reader.poll_recv_group(&waiter).is_pending());
				});
				drop((readers, track, broadcast));
			});
		}
	}
	bench.finish();
}

/// One reader holding `runs` cached groups it skipped as stale, each between two it handed
/// out, in a cache of `cached` groups, beside `readers` idle readers; see
/// [`bench_budget_widening`].
struct BudgetWidening {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	reader: track::Subscriber,
	_beside: Vec<track::Subscriber>,
	runs: u64,
}

impl BudgetWidening {
	/// A budget shorter than the 100ms between groups, and longer than wall-clock jitter.
	const NARROW: Duration = Duration::from_millis(50);

	fn new(readers: usize, cached: u64, runs: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let narrow = track::Subscription::default().with_max_delay(Self::NARROW);
		let mut reader = track.subscribe(narrow.clone());
		let beside = (0..readers).map(|_| track.subscribe(narrow.clone())).collect();
		let waiter = kio::Waiter::noop();
		let mut write = |sequences: std::ops::Range<u64>| {
			for sequence in sequences {
				let mut group = track.create_group(group::Info { sequence }).unwrap();
				group
					.write_frame(
						Timestamp::from_millis(100 * sequence).unwrap(),
						Bytes::from_static(b"x"),
					)
					.unwrap();
				group.finish().unwrap();
			}
			while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
				black_box(group);
			}
		};
		write(0..1);
		// Read three at a time, the first is stale: its successor starts a full budget behind
		// the third. The reader takes the other two, so each skip is a run of its own.
		for run in 0..runs {
			write(3 * run + 1..3 * run + 4);
		}
		for sequence in 3 * runs + 1..cached {
			write(sequence..sequence + 1);
		}
		Self {
			_broadcast: broadcast,
			_track: track,
			reader,
			_beside: beside,
			runs,
		}
	}

	fn widen(&mut self) {
		let wide = track::Subscription::default().with_max_delay(AGED_BUDGET);
		self.reader.update(wide).unwrap();
		let waiter = kio::Waiter::noop();
		let mut offered = 0;
		while let Poll::Ready(Ok(Some(group))) = self.reader.poll_recv_group(&waiter) {
			black_box(group);
			offered += 1;
		}
		assert_eq!(offered, self.runs);
	}
}

/// One reader widening its budget past the cached groups it skipped as stale, swept over the
/// readers beside it, the groups cached, and the runs it skipped. Each re-offered group must
/// cost the same, so a scan of the cache or the audience shows up as a slope.
fn bench_budget_widening(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_budget_widening");
	group.sample_size(10);
	for readers in [1usize, 64] {
		for cached in WIDENING_CACHED {
			for runs in [1u64, 16, 256].into_iter().filter(|runs| 4 * runs <= cached) {
				group.throughput(Throughput::Elements(runs));
				group.bench_function(BenchmarkId::new(format!("{readers}r/{cached}c"), runs), |b| {
					b.iter_batched_ref(
						|| BudgetWidening::new(readers, cached, runs),
						BudgetWidening::widen,
						BatchSize::PerIteration,
					);
				});
			}
		}
	}
	group.finish();
}

/// Readers whose range ends at a time no group reaches yet, as groups land behind a missing
/// sequence; see [`bench_time_end`].
struct TimeEnd {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	readers: Vec<track::Subscriber>,
	groups: u64,
}

impl TimeEnd {
	fn new(readers: usize, groups: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let mut zero = track.create_group(group::Info { sequence: 0 }).unwrap();
		zero.write_frame(Timestamp::from_millis(0).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		zero.finish().unwrap();
		// Group 1 never arrives, and the budget outlasts the run, so the end never settles.
		let subscription = track::Subscription::default()
			.with_max_delay(AGED_BUDGET)
			.with_ranges([track::Range {
				start: track::Anchor::Absolute(0),
				end: Some(track::Anchor::Time(Timestamp::from_secs(u32::MAX.into()).unwrap())),
			}]);
		let waiter = kio::Waiter::noop();
		let readers = (0..readers)
			.map(|_| {
				let mut reader = track.subscribe(subscription.clone());
				while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
				reader
			})
			.collect();
		Self {
			_broadcast: broadcast,
			track,
			readers,
			groups,
		}
	}

	fn run(&mut self) {
		let waiter = kio::Waiter::noop();
		for sequence in 2..self.groups + 2 {
			let mut group = self.track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(sequence).unwrap(), Bytes::from_static(b"x"))
				.unwrap();
			group.finish().unwrap();
			for reader in &mut self.readers {
				let mut handed = 0;
				while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
					black_box(group);
					handed += 1;
				}
				assert_eq!(handed, 1);
			}
		}
	}
}

/// Groups landing one at a time behind a missing sequence, each drained by readers whose
/// time end it cannot settle, swept over the groups and the readers. Each group must cost
/// each reader the same, so a rescan behind the gap shows up as a slope over the groups.
fn bench_time_end(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_time_end");
	group.sample_size(10);
	for readers in [1usize, 8, 64] {
		for groups in [64u64, 512, 4096] {
			group.throughput(Throughput::Elements(groups * readers as u64));
			group.bench_function(BenchmarkId::new(format!("{readers}r"), groups), |b| {
				b.iter_batched_ref(|| TimeEnd::new(readers, groups), TimeEnd::run, BatchSize::PerIteration);
			});
		}
	}
	group.finish();
}

/// Readers missing the same runs of one track below where its feed starts, and the handler
/// they ask; see [`bench_fill`].
struct Fills {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	dynamic: track::Requests,
	readers: Vec<track::Subscriber>,
	runs: u64,
}

impl Fills {
	fn new(readers: usize, runs: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", timed()).unwrap();
		let dynamic = track.requests();
		// The feed starts past every run, so each is a miss asked at once.
		let start = runs * 2;
		track.start_at(start).unwrap();
		let mut group = track.create_group(group::Info { sequence: start }).unwrap();
		group
			.write_frame(Timestamp::from_millis(start).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		group.finish().unwrap();
		// One range per run, with a sequence between each that no reader asks for.
		let ranges = (0..runs)
			.map(|run| track::Range::groups(run * 2..run * 2 + 1))
			.chain([track::Range::groups(start..)]);
		let subscription = track::Subscription::default()
			.with_ranges(ranges)
			.with_max_delay(AGED_BUDGET);
		let readers = (0..readers).map(|_| track.subscribe(subscription.clone())).collect();
		Self {
			_broadcast: broadcast,
			_track: track,
			dynamic,
			readers,
			runs,
		}
	}

	/// Every reader reads what is cached and asks for its misses, then raises its priority,
	/// which reaches every fill it waits on. The handler drops each fill they share, and
	/// every reader settles the drops.
	fn run(&mut self) {
		let waiter = kio::Waiter::noop();
		for reader in &mut self.readers {
			while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
				black_box(group);
			}
		}
		for reader in &mut self.readers {
			reader.update(reader.subscription().with_priority(1)).unwrap();
			assert!(reader.poll_recv_group(&waiter).is_pending());
		}
		let mut fills = 0;
		while let Poll::Ready(Ok(fill)) = self.dynamic.poll_requested_fill(&waiter) {
			assert_eq!(fill.priority(), 1, "the update reached the fill");
			fill.drop_groups(fill.start().group..fill.end());
			fills += 1;
		}
		assert_eq!(fills, self.runs, "readers share each run's fill");
		for reader in &mut self.readers {
			assert!(reader.poll_recv_group(&waiter).is_pending());
		}
	}
}

/// Readers missing the same runs below the feed's start, each asked as one fill they share,
/// swept over the readers and the runs. Each reader must pay the same per run, so a cost
/// that grows with the audience or the runs shows up as a slope.
fn bench_fill(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_fill");
	group.sample_size(10);
	for readers in [1usize, 8, 64] {
		for runs in [1u64, 64, 1024] {
			group.throughput(Throughput::Elements(runs * readers as u64));
			group.bench_function(BenchmarkId::new(format!("{readers}r"), runs), |b| {
				b.iter_batched_ref(|| Fills::new(readers, runs), Fills::run, BatchSize::PerIteration);
			});
		}
	}
	group.finish();
}

/// One reader whose disjoint ranges all fall below where the feed starts, and the handler
/// that answers its fills; see [`bench_range_filling`].
struct RangeFills {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	dynamic: track::Requests,
	reader: track::Subscriber,
	groups: u64,
}

impl RangeFills {
	fn new(ranges: u64, span: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", timed()).unwrap();
		let dynamic = track.requests();
		// The feed starts past every range, so each is a miss asked at once. A gap between
		// neighbors keeps every range its own fill.
		let start = ranges * (span + 1);
		track.start_at(start).unwrap();
		let mut group = track.create_group(group::Info { sequence: start }).unwrap();
		group
			.write_frame(Timestamp::from_millis(start).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		group.finish().unwrap();
		let subscription = track::Subscription::default()
			.with_max_delay(AGED_BUDGET)
			.with_ranges((0..ranges).map(|i| {
				let first = i * (span + 1);
				track::Range::groups(first..first + span)
			}));
		let reader = track.subscribe(subscription);
		Self {
			_broadcast: broadcast,
			_track: track,
			dynamic,
			reader,
			groups: ranges * span,
		}
	}

	/// The reader asks for its misses, the handler accepts every group of every fill, and the
	/// reader drains what they filled.
	fn run(&mut self) {
		let waiter = kio::Waiter::noop();
		assert!(
			self.reader.poll_recv_group(&waiter).is_pending(),
			"everything is a miss"
		);
		while let Poll::Ready(Ok(fill)) = self.dynamic.poll_requested_fill(&waiter) {
			while let Some(sequence) = fill.wanted() {
				let mut group = fill.accept(sequence, None).unwrap();
				group
					.write_frame(Timestamp::from_millis(sequence).unwrap(), Bytes::from_static(b"x"))
					.unwrap();
				group.finish().unwrap();
			}
		}
		let mut filled = 0;
		while let Poll::Ready(Ok(Some(group))) = self.reader.poll_recv_group(&waiter) {
			black_box(group);
			filled += 1;
		}
		assert_eq!(filled, self.groups);
	}
}

/// One reader filling disjoint ranges below the feed's start, swept over how many ranges and
/// how many groups each spans. Each group filled must cost the same, so a walk over the ranges
/// or the groups of each fill shows up as a slope over either axis.
fn bench_range_filling(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_range_filling");
	group.sample_size(10);
	for span in SERVED_SPAN {
		for ranges in SERVED_RANGES {
			group.throughput(Throughput::Elements(ranges * span));
			group.bench_function(BenchmarkId::new(format!("span_{span}"), ranges), |b| {
				b.iter_batched_ref(
					|| RangeFills::new(ranges, span),
					RangeFills::run,
					BatchSize::PerIteration,
				);
			});
		}
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_range_widening,
	bench_budget_widening,
	bench_fanout,
	bench_fanout_frame,
	bench_evict_fanout,
	bench_fetch_expiry,
	bench_fetch_budgets,
	bench_fetch_park,
	bench_parallel_write,
	bench_aborted_scan,
	bench_gc,
	bench_subscriber_churn,
	bench_subscriber_churn_after_peak,
	bench_subscriber_join,
	bench_range_demand,
	bench_subscription_ranges,
	bench_range_serving,
	bench_subscriber_preferences,
	bench_datagram_subscriber_join,
	bench_datagram_pressure,
	bench_parked_read,
	bench_sparse_read,
	bench_sparse_poll,
	bench_sparse_expire,
	bench_gap_flush,
	bench_wall,
	bench_empty_run,
	bench_time_end,
	bench_fill,
	bench_range_filling
);
criterion_main!(benches);
