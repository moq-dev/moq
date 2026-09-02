//! Producer-to-subscriber fanout benchmarks for the track model.
//!
//! The group benchmarks isolate per-frame storage. These benchmarks include the
//! track cache and subscription cursors around that storage: append a complete
//! group, notify every subscriber, and hand the cached group to each cursor.
//!
//! Run with `cargo bench -p moq-net --bench track`.

use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Timestamp, broadcast, cache, track};

/// Fanout sizes spanning a direct viewer, a small room, and a large room.
const FANOUT: [usize; 4] = [1, 8, 64, 512];

/// Small shared payload so the benchmark measures model overhead, not allocation.
const PAYLOAD: usize = 64;

/// Small enough to reach steady-state eviction during Criterion warm-up.
const CACHE_CAPACITY: u64 = 64 * 1024;

/// Concurrent publishers sharing one relay-style cache pool.
const WRITERS: [usize; 4] = [1, 2, 4, 8];

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
		info.origin.pool = cache::Pool::new(config);
		let mut broadcast = broadcast::Producer::new(info);
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
					info.origin.pool = pool;
					let mut broadcast = broadcast::Producer::new(info);
					let mut track = broadcast.create_track("bench", None).unwrap();
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

criterion_group!(benches, bench_fanout, bench_parallel_write);
criterion_main!(benches);
