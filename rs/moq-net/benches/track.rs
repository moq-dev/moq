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
//! `track_subscriber_churn_drops` drops one more run into a fragmented drop set read by wire
//! cursors, each reporting it, swept over the readers, the runs the track shares, the runs
//! each reader's range holds and already reported, and the span of each run (built with the
//! `fuzz` feature, which reaches the crate-private drop cursor). A drop must cost each reader
//! the runs it touches, so a slope over the runs, what each reader reported, or the span
//! means a cursor walks the set.
//!
//! `track_subscriber_join` measures a burst of viewers joining one track: each join
//! registers in the subscription list, so any per-join walk of it shows up as a slope.
//!
//! `track_parked_read` appends past a backlog of reads parked on their drift budget:
//! each append must wake only the reads it can expire, so waking the backlog shows up
//! as a slope over it.
//!
//! `track_sparse_read` hands out groups from a feed with every other key missing,
//! holding a fixed number of gaps the reader still remembers: each hand-out must cost the
//! same however many gaps it holds, so a walk over them shows up as a slope.
//! `track_sparse_poll` repeats a parked read over a feed holding deferred gaps, without
//! changing it: the repeat parks again without walking, so a slope over the gaps means a
//! repeated read revisits them again.
//!
//! `track_idle_poll` repeats every parked read over readers sharing one track, in arrival
//! and key order, with nothing new: a repeated read parks again without walking the
//! read path, so each costs a small constant, and a slope over the readers, or a jump in
//! that constant, means the early exit stopped applying.
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
//! `track_fetch_expiry` polls fetched readers expiring at the track's max age over
//! independent fanout and retained-history axes, so expiry work that scans either table
//! appears as a slope.
//!
//! `track_fetch_budgets` writes the frame that crosses every fetch's max-age deadline, over
//! live readers and finished fetches: deadlines the finished fetches left registered show up
//! as a slope over them.
//!
//! `track_fetch_park` parks one more fetch beside live readers waiting on the same max age,
//! so a park that sweeps every registered deadline shows up as a slope over the readers.
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
//! `track_time_end` lands groups one at a time behind a key that never arrives, each
//! drained by readers whose range ends at a time no group reaches, swept over the groups and
//! the readers: an unsettled time end walks on from where it stopped, so a rescan of the
//! groups behind the gap on every read shows up as a slope over them.
//!
//! `track_fill` has readers miss the same runs below where the feed starts, each asked of the
//! track's handler as a fill, swept over the readers and the runs: readers share each
//! fill, so asking, updating, dropping, and settling must cost each reader the same per
//! run, and a cost that grows with the audience or the runs beyond that shows up as a slope.
//!
//! `track_range_filling` has readers fill disjoint ranges below where the feed starts, the
//! filling counterpart of `track_range_serving`: one reader swept over how many ranges and how
//! many groups each spans, then the readers sharing those fills swept over the ranges. Each
//! group filled must cost each reader the same, so a walk over the ranges, the groups of each
//! fill, or the audience shows up as a slope.
//!
//! `track_fill_refusal` refuses a wide fill one group at a time, swept over the number of
//! refusals and readers. Each reader consumes each refusal before the next, so the cost per
//! refusal must not grow with the history of answers.
//!
//! `track_high_key` writes one group below a far-ahead key every reader already took, behind
//! a backlog of groups written there before, swept over the readers and the backlog: the
//! protected live edge must pin none of it, and a reader never hands out a group under its
//! floor, so a walk over the backlog on each write shows up as a slope over it. Every
//! iteration adds to the backlog too, so such a walk also lifts every point far above one
//! reader's constant.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench track`; `--features async` alone
//! leaves out `track_subscriber_churn_drops`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::fmt;
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::task::Poll;
use std::time::{Duration, Instant as WallInstant};

use moq_net::time::Instant;

use bytes::Bytes;
use moq_net::{Error, Timestamp, broadcast, cache, group, track};
use swole::{Iterations, bench};

/// One case's parameters, named as the case is.
#[derive(Clone)]
struct Case<T> {
	name: String,
	params: T,
}

impl<T> fmt::Display for Case<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(&self.name)
	}
}

fn case<T>(name: String, params: T) -> Case<T> {
	Case { name, params }
}

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default()
		.with_max_age(None)
		.with_timescale(moq_net::Timescale::MILLI)
}

/// Where the group at `key` starts on a [`timed`] track: groups start strictly in key order.
fn at(key: u64) -> Timestamp {
	Timestamp::from_millis(key).unwrap()
}

/// The groups from the one containing `start` through the one containing `end`.
fn span(start: u64, end: u64) -> track::Range {
	track::Range {
		start: track::Start::Time(start),
		end: Some(end),
	}
}

/// The groups from the one containing `start` onward.
fn from(start: u64) -> track::Range {
	track::Range {
		start: track::Start::Time(start),
		end: None,
	}
}

/// Fanout sizes spanning a direct viewer, a small room, and a large room.
const FANOUT: [usize; 4] = [1, 8, 64, 512];

/// Aborted-prefix sizes spanning a cache hit through a long stale scan.
const ABORTED: [usize; 4] = [1, 8, 64, 512];

/// Small shared payload so the benchmark measures model overhead, not allocation.
const PAYLOAD: usize = 64;

/// Small enough to reach steady-state eviction while warming up.
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

/// The budget a parked read's wall deadline is armed for, past any setup on the real clock:
/// a live join's `max`, or the max age a fetched read expires at.
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
		let track = broadcast
			.create_track("bench", timed().with_max_age(AGED_BUDGET))
			.unwrap();
		let mut open = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
		open.write_frame(Timestamp::ZERO, Bytes::from_static(b"f")).unwrap();
		for key in 1..=history as u64 {
			let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			group.write_frame(at(key), Bytes::from_static(b"f")).unwrap();
			group.finish().unwrap();
		}

		let consumer = track.consume();
		let waiter = kio::Waiter::noop();
		let mut fetched = Vec::with_capacity(readers);
		for _ in 0..readers {
			let mut pending = consumer.fetch_group(0, None);
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
	/// The next group's key.
	next: u64,
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
			next: 0,
		}
	}

	/// Append the next group, whose frames start at its key.
	fn append(&mut self) -> moq_net::group::Producer {
		let group = self
			.track
			.append_group(Timestamp::from_millis(self.next).unwrap())
			.unwrap();
		self.next += 1;
		group
	}

	/// Append one finished group and deliver its handle to every subscriber.
	fn cycle(&mut self) {
		let mut group = self.append();
		group.write_frame(at(group.start), self.payload.clone()).unwrap();
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

/// A track whose readers took a far-ahead key, as the publisher writes on below it.
struct HighKey {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	subscribers: Vec<track::Subscriber>,
	waiter: kio::Waiter,
	payload: Bytes,
	next: u64,
}

impl HighKey {
	fn new(subscribers: usize, backlog: usize) -> Self {
		let mut info = broadcast::Info::default();
		let config = cache::Config::default()
			.with_capacity(CACHE_CAPACITY)
			.with_expiry(cache::DEFAULT_EXPIRY);
		info.pool = cache::Pool::new(config);
		let broadcast = broadcast::Producer::new(info);
		let track = broadcast
			.create_track("bench", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		// Each reads from the far-ahead key on, so every later group lands below its floor.
		let mut subscribers: Vec<_> = (0..subscribers)
			.map(|_| track.subscribe(None).with_range(HIGH..))
			.collect();
		let waiter = kio::Waiter::noop();
		let mut high = Self {
			_broadcast: broadcast,
			track,
			subscribers: Vec::new(),
			waiter,
			payload: Bytes::from(vec![0; PAYLOAD]),
			next: 0,
		};
		high.write(HIGH);
		for subscriber in &mut subscribers {
			let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&high.waiter) else {
				unreachable!("the far-ahead group must be ready");
			};
			black_box(group);
		}
		high.subscribers = subscribers;
		for _ in 0..backlog {
			high.step();
		}
		high
	}

	fn write(&self, key: u64) {
		let mut group = self.track.create_group(moq_net::group::Info { start: key }).unwrap();
		group
			.write_frame(Timestamp::unscaled(key), self.payload.clone())
			.unwrap();
		group.finish().unwrap();
	}

	/// Write the next group below the far-ahead key and wake every reader, none of which
	/// hands it out.
	fn step(&mut self) {
		self.write(self.next);
		self.next += 1;
		for subscriber in &mut self.subscribers {
			assert!(subscriber.poll_recv_group(&self.waiter).is_pending());
		}
	}
}

/// The far-ahead key `track_high_key` jumps to first.
const HIGH: u64 = 1 << 40;

/// Groups written below the far-ahead key before `track_high_key` measures.
const BACKLOG: [usize; 3] = [0, 1024, 16_384];

fn high_key_cases() -> Vec<Case<(usize, usize)>> {
	FANOUT
		.into_iter()
		.flat_map(|subscribers| {
			BACKLOG.map(|backlog| case(format!("{subscribers}r/{backlog}"), (subscribers, backlog)))
		})
		.collect()
}

fn high_key(case: Case<(usize, usize)>) -> HighKey {
	let (subscribers, backlog) = case.params;
	HighKey::new(subscribers, backlog)
}

#[bench(args = high_key_cases(), setup = high_key, items = |case: Case<(usize, usize)>| case.params.0)]
fn track_high_key(high: &mut HighKey) {
	high.step();
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
		let track = broadcast
			.create_track("bench", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		let mut stale = Vec::with_capacity(aborted);

		for key in 0..aborted as u64 {
			stale.push(track.append_group(Timestamp::unscaled(key)).unwrap());
		}
		track
			.append_group(Timestamp::unscaled(aborted as u64))
			.unwrap()
			.finish()
			.unwrap();
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
	waiter: kio::Waiter,
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
				let mut group = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
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
			waiter: kio::Waiter::noop(),
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
		let mut track = broadcast
			.create_track("bench", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
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
	/// A collection's own waiter.
	waiter: kio::Waiter,
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
				let track = broadcast
					.create_track(format!("bench{i}"), moq_net::track::Info::default().with_max_age(None))
					.unwrap();
				// Ascending, so the first read is the held group under the finished ones.
				let mut sub = track.subscribe(
					track::Subscription::default()
						.with_ranges([track::Range::from(track::Start::Live {
							min: Some(AGED_BUDGET),
							max: Some(AGED_BUDGET),
						})])
						.with_order(track::Order::Ascending),
				);
				let mut held = track.append_group(Timestamp::unscaled(0)).unwrap();
				held.write_frame(moq_net::Timestamp::unscaled(held.start), Bytes::from_static(b"x"))
					.unwrap();
				for key in 1..groups.max(2) as u64 {
					let mut group = track.append_group(Timestamp::unscaled(key)).unwrap();
					group
						.write_frame(moq_net::Timestamp::unscaled(group.start), Bytes::from_static(b"x"))
						.unwrap();
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
			waiter: kio::Waiter::noop(),
		}
	}
}

fn wall_cases() -> Vec<Case<(usize, usize)>> {
	AGED_TRACKS
		.into_iter()
		.flat_map(|tracks| AGED_GROUPS.map(|groups| (tracks, groups)))
		// Skip the largest table: setup alone would dominate the run.
		.filter(|(tracks, groups)| tracks * groups <= 1_000_000)
		.map(|(tracks, groups)| case(format!("{tracks}x{groups}"), (tracks, groups)))
		.collect()
}

fn aged(case: Case<(usize, usize)>) -> Aged {
	let (tracks, groups) = case.params;
	Aged::new(tracks, groups)
}

mod track_wall {
	use std::hint::black_box;

	use moq_net::time::Instant;
	use swole::bench;

	use super::{AGED_BUDGET, Aged, Case, aged, wall_cases};

	/// A collection with every deadline armed and none due: every origin poll.
	#[bench(args = wall_cases(), setup = aged, items = |_| 1)]
	fn idle(aged: &Aged) -> Option<Instant> {
		aged.pool.poll(aged.now, &aged.waiter)
	}

	/// Re-polling one parked read re-arms its deadline.
	#[bench(args = wall_cases(), setup = aged, items = |_| 1)]
	fn repoll(aged: &mut Aged) {
		let (read, waiter) = &mut aged.reads[0];
		assert!(black_box(read.poll_read_frame(waiter)).is_pending());
	}

	/// The collection that releases every parked read at once. Its cost is per track released,
	/// so the cached groups behind each read are not swept.
	fn due_cases() -> Vec<Case<(usize, usize)>> {
		wall_cases().into_iter().filter(|case| case.params.1 == 1).collect()
	}

	#[bench(args = due_cases(), setup = aged, items = |case: Case<(usize, usize)>| case.params.0)]
	fn due(aged: Aged) -> Aged {
		// Past every deadline, which setup armed after `now`.
		black_box(aged.pool.poll(aged.now.saturating_add(2 * AGED_BUDGET), &aged.waiter));
		aged
	}
}

mod track_gc {
	use std::time::Duration;

	use moq_net::{cache, time::Instant};
	use swole::bench;

	use super::{SWEPT, Swept};

	/// Between passes, which is every origin poll: should stay flat as tracks grow.
	#[bench(args = SWEPT, setup = Swept::new, items = |_| 1)]
	fn idle(swept: &Swept) -> Option<Instant> {
		swept.pool.poll(swept.now, &swept.waiter)
	}

	/// Also advance supplied time, so reusing an identical instant cannot hide clock work.
	#[bench(args = SWEPT, setup = Swept::new, items = |_| 1)]
	fn advancing(swept: &mut Swept) -> Option<Instant> {
		swept.now = swept.now.saturating_add(Duration::from_nanos(1));
		swept.pool.poll(swept.now, &swept.waiter)
	}

	/// A due pass, stepping one sweep interval per call.
	#[bench(args = SWEPT, setup = Swept::new, items = |tracks| tracks)]
	fn sweep(swept: &mut Swept) -> Option<Instant> {
		swept.now = swept.now.saturating_add(cache::DEFAULT_EXPIRY / 2);
		swept.pool.poll(swept.now, &swept.waiter)
	}
}

#[bench(args = FANOUT, setup = Fanout::new, items = |subscribers| subscribers)]
fn track_fanout_group(fanout: &mut Fanout) {
	fanout.cycle();
}

/// Groups written between reads in `track_evict_fanout`, each evicting one.
const EVICTED: [usize; 3] = [1, 8, 64];

/// A full pool's fanout, and the groups each call writes into it.
struct EvictFanout {
	fanout: Fanout,
	evicted: usize,
}

impl EvictFanout {
	fn new(case: Case<(usize, usize)>) -> Self {
		let (subscribers, evicted) = case.params;
		let mut fanout = Fanout::new(subscribers);
		// Twice what the pool holds, so every write evicts.
		for _ in 0..2 * CACHE_CAPACITY / PAYLOAD as u64 {
			fanout.cycle();
		}
		Self { fanout, evicted }
	}

	/// The reads that judge each written group cached again, outside the measurement.
	fn read(&mut self) {
		let fanout = &mut self.fanout;
		for (subscriber, waiter) in fanout.subscribers.iter_mut().zip(&fanout.waiters) {
			while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(waiter) {
				black_box(group);
			}
		}
	}
}

fn evict_fanout_cases() -> Vec<Case<(usize, usize)>> {
	FANOUT
		.into_iter()
		.flat_map(|subscribers| {
			EVICTED.map(|evicted| case(format!("{subscribers}r/{evicted}"), (subscribers, evicted)))
		})
		.collect()
}

/// Only the writes and the evictions they cause are measured.
#[bench(
	args = evict_fanout_cases(),
	setup = EvictFanout::new,
	after = EvictFanout::read,
	items = |case: Case<(usize, usize)>| case.params.1,
)]
fn track_evict_fanout(evict: &mut EvictFanout) {
	for _ in 0..evict.evicted {
		let mut group = evict.fanout.append();
		group
			.write_frame(at(group.start), evict.fanout.payload.clone())
			.unwrap();
		group.finish().unwrap();
	}
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

/// Subscribers parked on an open group, re-polled only when a frame wakes them.
struct FanoutFrame {
	fanout: Fanout,
	queue: Arc<std::sync::Mutex<Vec<usize>>>,
	waiters: Vec<kio::Waiter>,
	open: Option<moq_net::group::Producer>,
	written: usize,
}

impl FanoutFrame {
	fn new(subscribers: usize) -> Self {
		let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
		let waiters = (0..subscribers)
			.map(|index| {
				let fired = Arc::new(Fired {
					index,
					queue: queue.clone(),
				});
				kio::Waiter::new(futures::task::waker(fired))
			})
			.collect();
		Self {
			fanout: Fanout::new(subscribers),
			queue,
			waiters,
			open: None,
			written: FRAMES_PER_GROUP,
		}
	}

	/// Roll to a new group once the open one is full, outside the measurement.
	fn roll(&mut self) {
		if self.written < FRAMES_PER_GROUP {
			return;
		}
		if let Some(open) = self.open.take() {
			open.finish().unwrap();
		}
		self.open = Some(self.fanout.append());
		self.written = 0;
		for (subscriber, waiter) in self.fanout.subscribers.iter_mut().zip(&self.waiters) {
			assert!(matches!(subscriber.poll_recv_group(waiter), Poll::Ready(Ok(Some(_)))));
			assert!(subscriber.poll_recv_group(waiter).is_pending());
		}
		self.queue.lock().unwrap().clear();
	}
}

/// Only the frame write and the re-polls it causes are measured, not the group roll.
#[bench(args = FANOUT, setup = FanoutFrame::new, before = FanoutFrame::roll)]
fn track_fanout_frame(frame: &mut FanoutFrame) {
	let group = frame.open.as_mut().unwrap();
	group
		.write_frame(at(group.start), frame.fanout.payload.clone())
		.unwrap();
	// A woken reader re-polls, as its task would.
	let woken = std::mem::take(&mut *frame.queue.lock().unwrap());
	for index in woken {
		black_box(
			frame.fanout.subscribers[index]
				.poll_recv_group(&frame.waiters[index])
				.is_pending(),
		);
	}
	frame.written += 1;
}

/// A track with groups presenting at 0..4 s and the last still open, `readers` fetches of
/// group 2 parked on its expiry at the track's 1.5 s max age, and `history` finished fetches
/// of it.
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
		let track = broadcast
			.create_track("bench", timed().with_max_age(Duration::from_millis(1_500)))
			.unwrap();
		for key in (0..4u64).map(|second| second * 1000) {
			let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			group.write_frame(at(key), Bytes::from_static(b"f")).unwrap();
			group.finish().unwrap();
		}
		let mut open = track.append_group(Timestamp::from_millis(4000).unwrap()).unwrap();
		open.write_frame(at(4000), Bytes::from_static(b"f")).unwrap();

		let mut fetched = Self {
			_broadcast: broadcast,
			consumer: track.consume(),
			_track: track,
			open,
			_readers: Vec::with_capacity(readers),
		};
		for _ in 0..history {
			drop(fetched.fetch());
		}
		fetched._readers = (0..readers).map(|_| fetched.fetch()).collect();
		fetched
	}

	/// Fetch the group at 2 seconds, returning the waiter that keeps its deadlines registered.
	fn fetch(&self) -> kio::Waiter {
		let waiter = kio::Waiter::noop();
		let mut pending = self.consumer.fetch_group(2000, None);
		assert!(matches!(kio::Task::poll(&mut *pending, &waiter), Poll::Ready(Ok(_))));
		waiter
	}
}

fn fetch_park(readers: usize) -> FetchBudgets {
	FetchBudgets::new(readers, 0)
}

/// One more fetch beside `readers` live ones: parking it must not scan the deadlines the live
/// readers hold, so a sweep per park shows up as a slope.
#[bench(args = [1, 64, 1_024], setup = fetch_park)]
fn track_fetch_park(fetched: &FetchBudgets) -> kio::Waiter {
	fetched.fetch()
}

fn fetch_budgets_cases() -> Vec<Case<(usize, usize)>> {
	[0, 1_024, 16_384]
		.into_iter()
		.flat_map(|history| [1, 64].map(|readers| case(format!("history_{history}/{readers}"), (readers, history))))
		.collect()
}

fn fetch_budgets(case: Case<(usize, usize)>) -> FetchBudgets {
	let (readers, history) = case.params;
	FetchBudgets::new(readers, history)
}

/// The write that crosses every fetch's deadline, beside live readers and the
/// finished fetches before them: the write must cost what the live readers do, so deadlines
/// left indexed by fetches that already finished show up as a slope over the history.
#[bench(args = fetch_budgets_cases(), setup = fetch_budgets)]
fn track_fetch_budgets(mut fetched: FetchBudgets) -> FetchBudgets {
	fetched
		.open
		.write_frame(Timestamp::from_secs(10).unwrap(), Bytes::from_static(b"f"))
		.unwrap();
	fetched
}

fn fetch_expiry_cases() -> Vec<Case<(usize, usize)>> {
	FETCH_HISTORY
		.into_iter()
		.flat_map(|history| FANOUT.map(|readers| case(format!("history_{history}/{readers}"), (readers, history))))
		.collect()
}

fn fetch_readers(case: Case<(usize, usize)>) -> FetchReaders {
	let (readers, history) = case.params;
	FetchReaders::new(readers, history)
}

#[bench(args = fetch_expiry_cases(), setup = fetch_readers, items = |case: Case<(usize, usize)>| case.params.0)]
fn track_fetch_expiry(fetched: &mut FetchReaders) {
	fetched.poll();
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
					for key in 0..iterations {
						let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
						group.write_frame(at(key), payload.clone()).unwrap();
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

fn shared_pool(writers: usize) -> (cache::Pool, usize) {
	let config = cache::Config::default()
		.with_capacity(CACHE_CAPACITY)
		.with_expiry(cache::DEFAULT_EXPIRY);
	(cache::Pool::new(config), writers)
}

/// Each iteration is one frame write, wherever it landed, so the per-iteration time is already
/// the number to compare across writer counts. Timed by the writers' span, without their setup.
#[bench(args = WRITERS, setup = shared_pool)]
fn track_parallel_write(input: &(cache::Pool, usize), iterations: Iterations) -> Duration {
	let (pool, writers) = input;
	parallel_write(pool, *writers, iterations.get())
}

#[bench(args = ABORTED, setup = AbortedScan::new, items = |aborted| aborted)]
fn track_aborted_scan(scan: &AbortedScan) {
	scan.scan();
}

fn churn_cases() -> Vec<Case<(usize, usize)>> {
	STEADY
		.into_iter()
		.flat_map(|steady| CHURN.map(|churn| case(format!("steady_{steady}/{churn}"), (steady, churn))))
		.collect()
}

fn churn(case: Case<(usize, usize)>) -> (Churn, usize) {
	let (steady, churn) = case.params;
	(Churn::new(steady, 0), churn)
}

#[bench(args = churn_cases(), setup = churn, items = |case: Case<(usize, usize)>| case.params.1)]
fn track_subscriber_churn(input: &mut (Churn, usize)) {
	let (setup, churn) = input;
	setup.run(*churn);
}

/// Viewers that join and leave in one `track_subscriber_churn_after_peak` call.
const AFTER_PEAK_CHURN: usize = 512;

fn after_peak(peak: usize) -> Churn {
	Churn::new(1, peak)
}

#[bench(args = PEAK, setup = after_peak, items = |_| AFTER_PEAK_CHURN)]
fn track_subscriber_churn_after_peak(setup: &mut Churn) {
	setup.run(AFTER_PEAK_CHURN);
}

/// Readers of a fragmented drop set, its runs, the runs each reader reported, and each run's span.
#[cfg(feature = "fuzz")]
const DROP_READERS: [usize; 3] = [1, 16, 256];
#[cfg(feature = "fuzz")]
const DROP_RUNS: [u64; 3] = [16, 1024, 65_536];
#[cfg(feature = "fuzz")]
const DROP_SPAN: [u64; 2] = [1, 1 << 32];

#[cfg(feature = "fuzz")]
mod drops {
	use moq_net::fuzz::DropSet;

	use super::{Case, DROP_READERS, DROP_RUNS, DROP_SPAN, case};

	pub(super) fn cases() -> Vec<Case<(usize, u64, u64, u64)>> {
		let mut cases = Vec::new();
		for readers in DROP_READERS {
			for runs in DROP_RUNS {
				for reported in [1, runs] {
					for span in DROP_SPAN {
						let name = format!("{readers}r/{runs}n/{reported}k/span_{span}");
						cases.push(case(name, (readers, runs, reported, span)));
					}
				}
			}
		}
		cases
	}

	pub(super) fn drop_set(case: Case<(usize, u64, u64, u64)>) -> DropSet {
		let (readers, runs, reported, span) = case.params;
		DropSet::new(readers, runs, reported, span)
	}
}

/// `hold` restores the runs and records each drop adds, outside the measurement.
#[cfg(feature = "fuzz")]
#[bench(args = drops::cases(), setup = drops::drop_set, before = moq_net::fuzz::DropSet::hold, items = |_| 1)]
fn track_subscriber_churn_drops(set: &mut moq_net::fuzz::DropSet) {
	set.drop_one();
}

fn steady(steady: usize) -> Churn {
	Churn::new(steady, 0)
}

/// The departure after each join and its aggregate poll, outside the measurement.
fn rejoin(setup: &mut Churn) {
	assert!(setup.track.poll_subscription_changed(&setup.waiter).is_pending());
}

/// One join and its aggregate poll; the viewer it returns leaves before the next.
#[bench(args = STEADY, setup = steady, after = rejoin, items = |_| 1)]
fn track_subscriber_join(setup: &mut Churn) -> track::Subscriber {
	let viewer = setup.track.subscribe(None);
	assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_pending());
	viewer
}

/// Preference changes and an extreme holder leaving, over the already held audience.
///
/// Each held reader asks for distinct values, so the aggregate's tables grow with the audience.
mod track_subscriber_preferences {
	use std::hint::black_box;
	use std::time::Duration;

	use moq_net::track;
	use swole::bench;

	use super::{CHURN, Case, Churn, STEADY, case, span};

	fn ordinary() -> track::Subscription {
		track::Subscription::default()
			.with_ranges([span(100, 199)])
			.with_order(track::Order::Ascending)
	}

	fn held(i: usize) -> track::Subscription {
		let i = i as u64;
		track::Subscription::default()
			.with_priority(((i % 255) as i16 - 128) as i8)
			.with_ranges([span(100 + i, 199 + i)])
			.with_order(track::Order::Ascending)
	}

	fn extreme() -> track::Subscription {
		track::Subscription::default()
			.with_priority(i8::MAX)
			.with_ranges([track::Range::from(track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			})])
	}

	/// The steady audience, each holding its own distinct preferences, caught up.
	fn audience(steady: usize) -> Churn {
		let mut setup = Churn::new(steady, 0);
		for (i, subscriber) in setup._steady.iter_mut().enumerate() {
			subscriber.update(held(i)).unwrap();
		}
		setup
	}

	struct Update {
		setup: Churn,
		viewer: track::Subscriber,
		ordinary: track::Subscription,
		extreme: track::Subscription,
		touched: usize,
	}

	fn update_cases() -> Vec<Case<(usize, usize)>> {
		STEADY
			.into_iter()
			.flat_map(|steady| CHURN.map(|touched| case(format!("{steady}/{touched}"), (steady, touched))))
			.collect()
	}

	fn update_setup(case: Case<(usize, usize)>) -> Update {
		let (steady, touched) = case.params;
		let mut setup = audience(steady);
		let viewer = setup.track.subscribe(ordinary());
		assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
		Update {
			setup,
			viewer,
			ordinary: ordinary(),
			extreme: extreme(),
			touched,
		}
	}

	#[bench(args = update_cases(), setup = update_setup, items = |case: Case<(usize, usize)>| 2 * case.params.1)]
	fn update(update: &mut Update) {
		let setup = &mut update.setup;
		for _ in 0..update.touched {
			update.viewer.update(update.extreme.clone()).unwrap();
			assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
			update.viewer.update(update.ordinary.clone()).unwrap();
			assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
		}
	}

	struct Leave {
		setup: Churn,
		extreme: track::Subscription,
		viewer: Option<track::Subscriber>,
	}

	fn leave_setup(steady: usize) -> Leave {
		let mut setup = audience(steady);
		assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
		Leave {
			setup,
			extreme: extreme(),
			viewer: None,
		}
	}

	/// The extreme holder joins and the aggregate takes it in, outside the measurement.
	fn join(leave: &mut Leave) {
		leave.viewer = Some(leave.setup.track.subscribe(leave.extreme.clone()));
		assert!(
			leave
				.setup
				.track
				.poll_subscription_changed(&leave.setup.waiter)
				.is_ready()
		);
	}

	#[bench(args = STEADY, setup = leave_setup, before = join, items = |_| 1)]
	fn extreme_leave(leave: &mut Leave) {
		drop(leave.viewer.take());
		assert!(black_box(leave.setup.track.poll_subscription_changed(&leave.setup.waiter)).is_ready());
	}
}

/// One reader moving between ranges nobody else holds, beside the steady readers.
struct RangeDemand {
	setup: Churn,
	viewer: track::Subscriber,
}

/// Far past every held range, so each move adds one run and removes another.
fn away(offset: u64) -> track::Subscription {
	let group = (1 << 40) + offset;
	track::Subscription::default().with_ranges([span(group, group)])
}

fn range_demand_cases() -> Vec<Case<(usize, usize)>> {
	STEADY
		.into_iter()
		.flat_map(|steady| {
			DISTINCT
				.into_iter()
				.filter(move |&distinct| distinct <= steady)
				.map(move |distinct| case(format!("{steady}r/{distinct}"), (steady, distinct)))
		})
		.collect()
}

fn range_demand(case: Case<(usize, usize)>) -> RangeDemand {
	let (steady, distinct) = case.params;
	let mut setup = Churn::new(steady, 0);
	for (i, subscriber) in setup._steady.iter_mut().enumerate() {
		// Overlapping, so the held ranges union into one run.
		let start = (i % distinct) as u64 * 10;
		subscriber
			.update(track::Subscription::default().with_ranges([span(start, start + 999)]))
			.unwrap();
	}
	let viewer = setup.track.subscribe(away(0));
	assert!(setup.track.poll_subscription_changed(&setup.waiter).is_ready());
	RangeDemand { setup, viewer }
}

/// One reader moving between ranges of its own, over readers holding `distinct` ranges.
#[bench(args = range_demand_cases(), setup = range_demand, items = |_| 2)]
fn track_range_demand(demand: &mut RangeDemand) {
	let setup = &mut demand.setup;
	demand.viewer.update(away(2)).unwrap();
	assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
	demand.viewer.update(away(0)).unwrap();
	assert!(black_box(setup.track.poll_subscription_changed(&setup.waiter)).is_ready());
}

/// Subscribers each asking for a disjoint range of one track.
struct Ranges {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	_viewers: Vec<track::Subscriber>,
}

fn ranges_cases() -> Vec<Case<(u64, u64)>> {
	SPAN.into_iter()
		.flat_map(|width| RANGES.map(|ranges| case(format!("span_{width}/{ranges}"), (width, ranges))))
		.collect()
}

fn subscription_ranges(case: Case<(u64, u64)>) -> Ranges {
	let (width, ranges) = case.params;
	let broadcast = broadcast::Info::default().produce();
	let track = broadcast
		.create_track("bench", moq_net::track::Info::default().with_max_age(None))
		.unwrap();
	// A gap between neighbors keeps every range distinct in the union.
	let viewers: Vec<_> = (0..ranges)
		.map(|i| {
			let start = i * (width + 1);
			track.subscribe(track::Subscription::default().with_ranges([span(start, start + width - 1)]))
		})
		.collect();
	assert_eq!(track.subscription().unwrap().ranges.len(), ranges as usize);
	Ranges {
		_broadcast: broadcast,
		track,
		_viewers: viewers,
	}
}

#[bench(args = ranges_cases(), setup = subscription_ranges, items = |case: Case<(u64, u64)>| case.params.1)]
fn track_subscription_ranges(ranges: &Ranges) -> Option<track::Subscription> {
	ranges.track.subscription()
}

/// A cached track and a reader of disjoint ranges of it, subscribed fresh before each call.
struct RangeServing {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	subscription: track::Subscription,
	reader: Option<track::Subscriber>,
	served: u64,
}

impl RangeServing {
	fn new(case: Case<(u64, u64)>) -> Self {
		let (width, ranges) = case.params;
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast
			.create_track("bench", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		// A gap between neighbors keeps every range distinct, and its group cached.
		for key in 0..ranges * (width + 1) {
			let mut group = track.append_group(Timestamp::unscaled(key)).unwrap();
			group
				.write_frame(moq_net::Timestamp::unscaled(group.start), Bytes::from_static(b"x"))
				.unwrap();
			group.finish().unwrap();
		}
		let subscription = track::Subscription::default().with_ranges((0..ranges).map(|i| {
			let start = i * (width + 1);
			span(start, start + width - 1)
		}));
		Self {
			_broadcast: broadcast,
			track,
			subscription,
			reader: None,
			served: ranges * width,
		}
	}

	fn subscribe(&mut self) {
		self.reader = Some(self.track.subscribe(self.subscription.clone()));
	}
}

fn serving_cases() -> Vec<Case<(u64, u64)>> {
	SERVED_SPAN
		.into_iter()
		.flat_map(|width| SERVED_RANGES.map(|ranges| case(format!("span_{width}/{ranges}"), (width, ranges))))
		.collect()
}

/// Drains the fresh reader, which it returns so its drop stays outside the measurement.
#[bench(
	args = serving_cases(),
	setup = RangeServing::new,
	before = RangeServing::subscribe,
	items = |case: Case<(u64, u64)>| case.params.0 * case.params.1,
)]
fn track_range_serving(serving: &mut RangeServing) -> track::Subscriber {
	let mut reader = serving.reader.take().unwrap();
	let waiter = kio::Waiter::noop();
	let mut served = 0;
	while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
		black_box(group);
		served += 1;
	}
	assert_eq!(served, serving.served);
	reader
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
/// It times itself because its budget must cover every measured append, sized by the iterations.
#[bench(args = [8, 64, 512], items = |_| 1)]
fn track_parked_read(parked: usize, iterations: Iterations) -> Duration {
	const MICROS_PER_GROUP: u64 = 2500;
	// A group's start on the millisecond track, from its first frame's microseconds.
	let millis = |micros: u64| micros / 1000;
	let iterations = iterations.get();
	let broadcast = broadcast::Info::default().produce();
	let track = broadcast.create_track("bench", timed()).unwrap();
	// Cover setup and every measured append so the same backlog stays parked.
	let max_delay = Duration::from_micros(
		iterations
			.checked_add(parked as u64)
			.unwrap()
			.checked_mul(MICROS_PER_GROUP)
			.unwrap(),
	);
	let mut sub = track.subscribe(track::Subscription::default().with_ranges([track::Range::from(
		track::Start::Live {
			min: Some(max_delay),
			max: Some(max_delay),
		},
	)]));
	let mut micros = 0;
	let mut open = Vec::with_capacity(parked);
	let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
	let mut reads: Vec<_> = (0..parked)
		.map(|read| {
			let mut group = track
				.append_group(Timestamp::from_millis(millis(micros)).unwrap())
				.unwrap();
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
		let mut next = track
			.append_group(Timestamp::from_millis(millis(micros)).unwrap())
			.unwrap();
		next.write_frame(Timestamp::from_micros(micros).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		next.finish().unwrap();
		micros += MICROS_PER_GROUP;
		repoll(&mut reads);
	}
	start.elapsed()
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
	/// The waiter a repeated parked read polls with.
	waiter: kio::Waiter,
}

impl Sparse {
	fn new(info: track::Info, gaps: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", info.clone()).unwrap();
		let sub = track.subscribe(track::Subscription::default().with_ranges([track::Range::from(
			track::Start::Live {
				min: Some(Duration::from_secs(3600)),
				max: Some(Duration::from_secs(3600)),
			},
		)]));
		let mut sparse = Self {
			_broadcast: broadcast,
			track,
			sub,
			next: gaps * 2,
			missing: 1,
			timed: info.timescale.is_some(),
			waiter: kio::Waiter::noop(),
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

	fn hand_out(&mut self, key: u64) {
		let timestamp = match self.timed {
			true => at(key),
			false => moq_net::Timestamp::unscaled(key),
		};
		let mut group = self.track.create_group(moq_net::group::Info { start: key }).unwrap();
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

fn untimed_sparse(gaps: u64) -> Sparse {
	Sparse::new(track::Info::default().with_max_age(None), gaps)
}

fn timed_sparse(gaps: u64) -> Sparse {
	Sparse::new(timed(), gaps)
}

/// Gaps a sparse feed holds.
const GAPS: [u64; 3] = [16, 256, 4096];

mod track_sparse_read {
	use swole::bench;

	use super::{GAPS, Sparse, timed_sparse, untimed_sparse};

	#[bench(args = GAPS, setup = untimed_sparse, items = |_| 2)]
	fn untimed(sparse: &mut Sparse) {
		sparse.read_pair();
	}

	#[bench(args = GAPS, setup = timed_sparse, items = |_| 2)]
	fn timed(sparse: &mut Sparse) {
		sparse.read_pair();
	}
}

mod track_sparse_poll {
	use std::hint::black_box;

	use swole::bench;

	use super::{GAPS, Sparse, timed_sparse, untimed_sparse};

	#[bench(args = GAPS, setup = untimed_sparse)]
	fn untimed(sparse: &mut Sparse) {
		assert!(black_box(sparse.sub.poll_recv_group(&sparse.waiter)).is_pending());
	}

	#[bench(args = GAPS, setup = timed_sparse)]
	fn timed(sparse: &mut Sparse) {
		assert!(black_box(sparse.sub.poll_recv_group(&sparse.waiter)).is_pending());
	}
}

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
		let mut open = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
		open.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		let waiter = kio::Waiter::noop();
		let (mut recv, mut next) = (Vec::new(), Vec::new());
		for _ in 0..readers {
			let reader = track.subscribe(track::Subscription::default().with_ranges([track::Range::from(
				track::Start::Live {
					min: Some(AGED_BUDGET),
					max: Some(AGED_BUDGET),
				},
			)]));
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

mod track_idle_poll {
	use swole::bench;

	use super::{FANOUT, Idle};

	fn recv_readers(readers: usize) -> Idle {
		Idle::new(readers, false)
	}

	fn next_readers(readers: usize) -> Idle {
		Idle::new(readers, true)
	}

	#[bench(args = FANOUT, setup = recv_readers, items = |readers| readers)]
	fn recv(idle: &mut Idle) {
		idle.poll();
	}

	#[bench(args = FANOUT, setup = next_readers, items = |readers| readers)]
	fn next(idle: &mut Idle) {
		idle.poll();
	}
}

/// A sparse feed that opens one gap per millisecond, keeping `gaps` within its budget.
struct Expiring {
	sparse: Sparse,
	pool: cache::Pool,
	now: Instant,
}

impl Expiring {
	fn new(gaps: u64) -> Self {
		let mut sparse = Sparse::new(track::Info::default().with_max_age(None), 0);
		sparse
			.sub
			.update(
				track::Subscription::default().with_ranges([track::Range::from(track::Start::Live {
					min: Some(Duration::from_millis(gaps)),
					max: Some(Duration::from_millis(gaps)),
				})]),
			)
			.unwrap();
		let pool = sparse._broadcast.info().pool.clone();
		// Supply time ahead of the host clock, so only these steps age the gaps even
		// under an instruction profiler. No cache capacity or idle expiry is enabled.
		let mut fixture = Self {
			sparse,
			pool,
			now: Instant::now().saturating_add(Duration::from_secs(3600)),
		};
		for gap in 0..gaps {
			fixture.pool.poll(fixture.now, &kio::Waiter::noop());
			fixture.sparse.hand_out(gap * 2);
			fixture.now = fixture.now.saturating_add(Duration::from_millis(1));
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
		self.now = self.now.saturating_add(Duration::from_millis(1));
		self.pool.poll(self.now, &kio::Waiter::noop());
		self.sparse.hand_out(self.sparse.next);
		self.sparse.next += 2;
	}
}

#[bench(args = GAPS, setup = Expiring::new)]
fn track_sparse_expire(expiring: &mut Expiring) {
	expiring.arrive();
}

/// A reader remembering `gaps` gaps, each below a handed-out group since aborted but still
/// cached, then a stamped survivor after them all.
struct GapFlush {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	sub: track::Subscriber,
}

impl GapFlush {
	fn new(gaps: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		let mut sub = track.subscribe(track::Subscription::default().with_ranges([track::Range::from(
			track::Start::Live {
				min: Some(Duration::from_secs(3600)),
				max: Some(Duration::from_secs(3600)),
			},
		)]));
		let mut hand_out = |key: u64| {
			let mut group = track.create_group(moq_net::group::Info { start: key }).unwrap();
			group.write_frame(at(key), Bytes::from_static(b"x")).unwrap();
			let Poll::Ready(Ok(Some(read))) = sub.poll_recv_group(&kio::Waiter::noop()) else {
				panic!("the group is cached");
			};
			black_box(read);
			group
		};
		// Every odd key is missing.
		let handed: Vec<_> = (0..gaps).map(|gap| hand_out(gap * 2)).collect();
		hand_out(gaps * 2).finish().unwrap();
		for group in handed {
			group.abort(Error::Cancel).unwrap();
		}
		Self {
			_broadcast: broadcast,
			track,
			sub,
		}
	}

	/// Hand out a group two hours on, which puts every remembered gap past the budget.
	// A profiling boundary excludes publishing the groups and their aborts.
	#[inline(never)]
	fn flush(&mut self) {
		const TWO_HOURS: u64 = 7_200_000;
		let mut group = self
			.track
			.create_group(moq_net::group::Info { start: TWO_HOURS })
			.unwrap();
		group.write_frame(at(TWO_HOURS), Bytes::from_static(b"x")).unwrap();
		let Poll::Ready(Ok(Some(read))) = self.sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("the group is cached");
		};
		black_box(read);
	}
}

/// One hand-out forgetting every remembered gap at once, each below a group that was
/// aborted but is still cached, swept over the gaps. The groups judged share one
/// successor past the aborted run, so finding it once per gap shows up as a slope.
#[bench(args = [16u64, 64, 256, 512, 4096], setup = GapFlush::new, items = |gaps| gaps)]
fn track_gap_flush(mut flush: GapFlush) -> GapFlush {
	flush.flush();
	flush
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
		// A group at `key` milliseconds, with a frame there or none.
		let append = |key: u64, frame: bool| {
			let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			if frame {
				group.write_frame(at(key), Bytes::from_static(b"x")).unwrap();
			}
			group.finish().unwrap();
		};
		append(0, true);
		for key in 1..=run {
			append(key, false);
		}
		const HOUR: u64 = 3_600_000;
		let after = match mode {
			Run::Stale => vec![2 * HOUR, 4 * HOUR],
			Run::Fresh | Run::Retired => vec![run + 5],
		};
		for key in after {
			append(key, true);
		}
		let (budget, handed) = match mode {
			// The run's last group is written off, but two groups follow it.
			Run::Stale => (Duration::from_millis(HOUR), run as usize + 2),
			Run::Fresh => (Duration::from_millis(HOUR), run as usize + 2),
			Run::Retired => (Duration::ZERO, 1),
		};
		let budget = track::Subscription::default().with_ranges([track::Range::from(track::Start::Live {
			min: None,
			max: Some(budget),
		})]);
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

fn empty_run_cases() -> Vec<Case<(Run, usize, u64)>> {
	let mut cases = Vec::new();
	for (name, mode) in [("stale", Run::Stale), ("fresh", Run::Fresh), ("retired", Run::Retired)] {
		for readers in [1usize, 8, 64] {
			for run in [64u64, 512, 4096] {
				cases.push(case(format!("{name}/{readers}/{run}"), (mode, readers, run)));
			}
		}
	}
	cases
}

fn empty_run(case: Case<(Run, usize, u64)>) -> EmptyRun {
	let (mode, readers, run) = case.params;
	EmptyRun::new(run, readers, mode)
}

/// Readers draining a run of groups that finished without a frame, swept over the run and
/// the readers, with the groups after it past the budget (`stale`), within it (`fresh`), or
/// the run retired by the wall clock (`retired`).
/// A reader's cost must stay linear in the run, so a walk over the rest of the run per
/// group shows up as a slope over it.
#[bench(
	args = empty_run_cases(),
	setup = empty_run,
	items = |case: Case<(Run, usize, u64)>| case.params.2 * case.params.1 as u64,
)]
fn track_empty_run(mut run: EmptyRun) -> EmptyRun {
	run.drain();
	run
}

/// Idle narrow readers of a cached track, the first of which widens and narrows again.
struct RangeWidening {
	readers: Vec<track::Subscriber>,
	_track: track::Producer,
	_broadcast: broadcast::Producer,
	narrow: track::Subscription,
	wide: track::Subscription,
	waiter: kio::Waiter,
}

impl RangeWidening {
	fn new(case: Case<(usize, u64)>) -> Self {
		let (readers, cached) = case.params;
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast
			.create_track("bench", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		for key in 0..cached {
			let mut group = track.append_group(Timestamp::unscaled(key)).unwrap();
			group
				.write_frame(moq_net::Timestamp::unscaled(group.start), Bytes::from_static(b"x"))
				.unwrap();
			group.finish().unwrap();
		}
		let narrow = track::Subscription::default().with_ranges([span(0, 0)]);
		let mid = cached / 2;
		let wide = narrow.clone().with_ranges([span(0, 0), span(mid, mid + WIDENED - 1)]);
		let waiter = kio::Waiter::noop();
		let readers = (0..readers)
			.map(|_| {
				let mut reader = track.subscribe(narrow.clone());
				while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
				reader
			})
			.collect();
		Self {
			readers,
			_track: track,
			_broadcast: broadcast,
			narrow,
			wide,
			waiter,
		}
	}
}

fn range_widening_cases() -> Vec<Case<(usize, u64)>> {
	WIDENING_READERS
		.into_iter()
		.flat_map(|readers| WIDENING_CACHED.map(|cached| case(format!("{readers}r/{cached}"), (readers, cached))))
		.collect()
}

/// One reader widening its ranges by [`WIDENED`] cached groups mid-cache and back, beside
/// idle narrow readers. Every widening after the first finds groups already handed out,
/// which the reader skips, so each one costs the same scan of the span.
#[bench(args = range_widening_cases(), setup = RangeWidening::new, items = |_| 1)]
fn track_range_widening(widening: &mut RangeWidening) {
	let reader = &mut widening.readers[0];
	reader.update(widening.wide.clone()).unwrap();
	while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&widening.waiter) {
		black_box(group);
	}
	reader.update(widening.narrow.clone()).unwrap();
	// The narrow range is already delivered, so it ends until widened again.
	assert!(matches!(
		reader.poll_recv_group(&widening.waiter),
		Poll::Ready(Ok(None))
	));
}

/// One reader holding `runs` cached groups it skipped as stale, each between two it handed
/// out, in a cache of `cached` groups, beside `readers` idle readers; see
/// [`track_budget_widening`].
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
		let narrow = track::Subscription::default().with_ranges([track::Range::from(track::Start::Live {
			min: Some(Self::NARROW),
			max: Some(Self::NARROW),
		})]);
		let mut reader = track.subscribe(narrow.clone());
		let beside = (0..readers).map(|_| track.subscribe(narrow.clone())).collect();
		let waiter = kio::Waiter::noop();
		let mut write = |keys: std::ops::Range<u64>| {
			for key in keys {
				let mut group = track.create_group(group::Info { start: 100 * key }).unwrap();
				group.write_frame(at(100 * key), Bytes::from_static(b"x")).unwrap();
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
		for key in 3 * runs + 1..cached {
			write(key..key + 1);
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
		let wide = track::Subscription::default().with_ranges([track::Range::from(track::Start::Live {
			min: Some(AGED_BUDGET),
			max: Some(AGED_BUDGET),
		})]);
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

fn budget_widening_cases() -> Vec<Case<(usize, u64, u64)>> {
	let mut cases = Vec::new();
	for readers in [1usize, 64] {
		for cached in WIDENING_CACHED {
			for runs in [1u64, 16, 256].into_iter().filter(|runs| 4 * runs <= cached) {
				cases.push(case(format!("{readers}r/{cached}c/{runs}"), (readers, cached, runs)));
			}
		}
	}
	cases
}

fn budget_widening(case: Case<(usize, u64, u64)>) -> BudgetWidening {
	let (readers, cached, runs) = case.params;
	BudgetWidening::new(readers, cached, runs)
}

/// One reader widening its budget past the cached groups it skipped as stale, swept over the
/// readers beside it, the groups cached, and the runs it skipped. Each re-offered group must
/// cost the same, so a scan of the cache or the audience shows up as a slope.
#[bench(
	args = budget_widening_cases(),
	setup = budget_widening,
	items = |case: Case<(usize, u64, u64)>| case.params.2,
)]
fn track_budget_widening(mut widening: BudgetWidening) -> BudgetWidening {
	widening.widen();
	widening
}

/// Readers whose range ends at a time no group reaches yet, as groups land behind a missing
/// key; see [`track_time_end`].
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
		let mut zero = track.create_group(group::Info { start: 0 }).unwrap();
		zero.write_frame(Timestamp::from_millis(0).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		zero.finish().unwrap();
		// Group 1 never arrives, and the budget outlasts the run, so the end never settles.
		let subscription = track::Subscription::default().with_ranges([span(0, u64::from(u32::MAX) * 1000)]);
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
		for key in 2..self.groups + 2 {
			let mut group = self.track.create_group(group::Info { start: key }).unwrap();
			group
				.write_frame(Timestamp::from_millis(key).unwrap(), Bytes::from_static(b"x"))
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

fn time_end_cases() -> Vec<Case<(usize, u64)>> {
	[1usize, 8, 64]
		.into_iter()
		.flat_map(|readers| [64u64, 512, 4096].map(|groups| case(format!("{readers}r/{groups}"), (readers, groups))))
		.collect()
}

fn time_end(case: Case<(usize, u64)>) -> TimeEnd {
	let (readers, groups) = case.params;
	TimeEnd::new(readers, groups)
}

/// Groups landing one at a time behind a missing key, each drained by readers whose
/// time end it cannot settle, swept over the groups and the readers. Each group must cost
/// each reader the same, so a rescan behind the gap shows up as a slope over the groups.
#[bench(
	args = time_end_cases(),
	setup = time_end,
	items = |case: Case<(usize, u64)>| case.params.1 * case.params.0 as u64,
)]
fn track_time_end(mut end: TimeEnd) -> TimeEnd {
	end.run();
	end
}

/// Readers missing the same runs of one track below where its feed starts, and the handler
/// they ask; see [`track_fill`].
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
		// The feed starts past every run, so each is a miss.
		let start = runs * 2;
		track.start_at(start).unwrap();
		let mut group = track.create_group(group::Info { start }).unwrap();
		group
			.write_frame(Timestamp::from_millis(start).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		group.finish().unwrap();
		// One range per run, with a key between each that no reader asks for.
		let ranges = (0..runs).map(|run| span(run * 2, run * 2)).chain([from(start)]);
		let subscription = track::Subscription::default().with_ranges(ranges);
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
	/// which reaches every fill it waits on. A run past the first starts at a time below the
	/// feed's start, whose group each reader asks a fill of its own for first: the handler
	/// can't resolve times, so it closes each unanswered, and the readers then ask that run
	/// as a miss.
	/// The handler drops each fill they share, and every reader settles the drops.
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
		let (mut lookups, mut fills) = (0, 0);
		// The time starts go out first, and the misses they leave behind them.
		for _ in 0..2 {
			while let Poll::Ready(Ok(fill)) = self.dynamic.poll_requested_fill(&waiter) {
				assert_eq!(fill.priority(), 1, "the update reached the fill");
				match fill.floor() < fill.start().group {
					true => {
						drop(fill);
						lookups += 1;
					}
					false => {
						fill.drop_groups(fill.start().group..fill.end());
						fills += 1;
					}
				}
			}
			for reader in &mut self.readers {
				assert!(reader.poll_recv_group(&waiter).is_pending());
			}
		}
		let readers = self.readers.len() as u64;
		assert_eq!(
			lookups,
			(self.runs - 1) * readers,
			"each reader looks up each later run"
		);
		assert_eq!(fills, self.runs, "readers share each run's fill");
	}
}

fn fill_cases() -> Vec<Case<(usize, u64)>> {
	[1usize, 8, 64]
		.into_iter()
		.flat_map(|readers| [1u64, 64, 1024].map(|runs| case(format!("{readers}r/{runs}"), (readers, runs))))
		.collect()
}

fn fills(case: Case<(usize, u64)>) -> Fills {
	let (readers, runs) = case.params;
	Fills::new(readers, runs)
}

/// Readers missing the same runs below the feed's start, each asked as one fill they share,
/// swept over the readers and the runs. Each reader must pay the same per run, so a cost
/// that grows with the audience or the runs shows up as a slope.
#[bench(args = fill_cases(), setup = fills, items = |case: Case<(usize, u64)>| case.params.1 * case.params.0 as u64)]
fn track_fill(mut fills: Fills) -> Fills {
	fills.run();
	fills
}

/// Readers whose disjoint ranges all fall below where the feed starts, and the handler that
/// answers the fills they share; see [`track_range_filling`].
struct RangeFills {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	dynamic: track::Requests,
	readers: Vec<track::Subscriber>,
	groups: u64,
}

impl RangeFills {
	fn new(readers: usize, ranges: u64, width: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", timed()).unwrap();
		let dynamic = track.requests();
		// The feed starts past every range, so each is a miss. A gap before each range keeps
		// it its own fill, and has every range start at a time no group held places.
		let start = ranges * (width + 1) + 1;
		track.start_at(start).unwrap();
		let mut group = track.create_group(group::Info { start }).unwrap();
		group
			.write_frame(Timestamp::from_millis(start).unwrap(), Bytes::from_static(b"x"))
			.unwrap();
		group.finish().unwrap();
		let subscription = track::Subscription::default().with_ranges((0..ranges).map(|i| {
			let first = i * (width + 1) + 1;
			span(first, first + width - 1)
		}));
		let readers = (0..readers).map(|_| track.subscribe(subscription.clone())).collect();
		Self {
			_broadcast: broadcast,
			_track: track,
			dynamic,
			readers,
			groups: ranges * width,
		}
	}

	/// The readers ask for their misses, the handler accepts every group of every fill once,
	/// and each reader drains what they filled. Each range starts at a time below the feed's
	/// start, whose group each reader asks a fill of its own for first: the handler can't
	/// resolve times, so it closes each unanswered, and the readers then ask that range as a
	/// miss.
	fn run(&mut self) {
		let waiter = kio::Waiter::noop();
		for reader in &mut self.readers {
			assert!(reader.poll_recv_group(&waiter).is_pending(), "everything is a miss");
		}
		let mut filled = vec![0; self.readers.len()];
		// The time starts go out first, and the misses they leave behind them.
		for _ in 0..2 {
			while let Poll::Ready(Ok(fill)) = self.dynamic.poll_requested_fill(&waiter) {
				// A refusal would fail the reader instead.
				if fill.floor() < fill.start().group {
					continue;
				}
				while let Some(key) = fill.wanted() {
					let mut group = fill.accept(key, None).unwrap().expect("a fresh group");
					group
						.write_frame(Timestamp::from_millis(key).unwrap(), Bytes::from_static(b"x"))
						.unwrap();
					group.finish().unwrap();
				}
			}
			for (reader, filled) in self.readers.iter_mut().zip(&mut filled) {
				while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
					black_box(group);
					*filled += 1;
				}
			}
		}
		assert!(filled.iter().all(|filled| *filled == self.groups));
	}
}

fn range_filling_cases() -> Vec<Case<(usize, u64, u64)>> {
	let mut cases = Vec::new();
	for span in SERVED_SPAN {
		for ranges in SERVED_RANGES {
			cases.push(case(format!("span_{span}/{ranges}"), (1, ranges, span)));
		}
	}
	for readers in [8, 64] {
		for ranges in SERVED_RANGES {
			cases.push(case(format!("span_8/{readers}r/{ranges}"), (readers, ranges, 8)));
		}
	}
	cases
}

fn range_fills(case: Case<(usize, u64, u64)>) -> RangeFills {
	let (readers, ranges, span) = case.params;
	RangeFills::new(readers, ranges, span)
}

/// Readers filling disjoint ranges below the feed's start, swept over how many ranges and how
/// many groups each spans for one reader, then over the ranges and the readers sharing their
/// fills. Each group filled must cost each reader the same, so a walk over the ranges, the
/// groups of each fill, or the readers shows up as a slope over that axis.
#[bench(
	args = range_filling_cases(),
	setup = range_fills,
	items = |case: Case<(usize, u64, u64)>| case.params.1 * case.params.2 * case.params.0 as u64,
)]
fn track_range_filling(mut fills: RangeFills) -> RangeFills {
	fills.run();
	fills
}

/// A shared fill refused newest first, with each reader taking each answer as it arrives.
struct RefusedFill {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	_dynamic: track::Requests,
	fill: track::FillGroups,
	readers: Vec<track::Subscriber>,
}

impl RefusedFill {
	const START: u64 = 1 << 40;

	fn new(readers: usize) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", timed()).unwrap();
		let dynamic = track.requests();
		track.start_at(Self::START).unwrap();
		let mut group = track.create_group(group::Info { start: Self::START }).unwrap();
		group.write_frame(Timestamp::ZERO, Bytes::from_static(b"x")).unwrap();
		group.finish().unwrap();
		let waiter = kio::Waiter::noop();
		let readers = (0..readers)
			.map(|_| {
				let mut reader = track.subscribe(track::Subscription::default().with_ranges([from(0)]));
				let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) else {
					panic!("the live group is cached");
				};
				assert_eq!(group.start, Self::START);
				assert!(reader.poll_recv_group(&waiter).is_pending());
				reader
			})
			.collect();
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the miss below the start is asked");
		};
		assert!(
			dynamic.poll_requested_fill(&waiter).is_pending(),
			"readers share the fill"
		);
		Self {
			_broadcast: broadcast,
			_track: track,
			_dynamic: dynamic,
			fill,
			readers,
		}
	}

	fn run(&mut self, refusals: u64) {
		let waiter = kio::Waiter::noop();
		for refused in 1..=refusals {
			let key = self.fill.wanted().unwrap();
			self.fill.drop_groups(key..key + 1);
			for reader in &mut self.readers {
				assert!(reader.poll_recv_group(&waiter).is_pending());
			}
			assert_eq!(self.fill.wanted(), Some(Self::START - refused - 1));
		}
	}
}

fn fill_refusal_cases() -> Vec<Case<(usize, u64)>> {
	let mut cases: Vec<_> = [1usize, 8, 64]
		.into_iter()
		.flat_map(|readers| {
			[16u64, 256, 4096].map(|refusals| case(format!("{readers}r/{refusals}"), (readers, refusals)))
		})
		.collect();
	// Keep the long single-reader history that exposes even a cheap per-refusal copy.
	cases.push(case("1r/100000".to_string(), (1, 100_000)));
	cases
}

fn refused_fill(case: Case<(usize, u64)>) -> (RefusedFill, u64) {
	let (readers, refusals) = case.params;
	(RefusedFill::new(readers), refusals)
}

#[bench(
	args = fill_refusal_cases(),
	setup = refused_fill,
	items = |case: Case<(usize, u64)>| case.params.1 * case.params.0 as u64,
)]
fn track_fill_refusal(mut input: (RefusedFill, u64)) -> (RefusedFill, u64) {
	input.0.run(input.1);
	input
}

fn main() {
	swole::main();
}
