//! Viewer sessions on a relay's origin: each requests one broadcast through a hop
//! of its own, subscribes to its track, and reads the latest group. Viewers share a
//! broadcast's front, so neither what a viewer costs nor what one leaves behind
//! may depend on how many came before.
//!
//! Each viewer excludes its own hop the way a session serves its peer. That view
//! is crate-private, so this reaches it through the hidden `fuzz` module. Its own
//! target keeps the counting allocator out of the `origin` bench.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench viewers`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::FutureExt;
use moq_net::{Hop, Timestamp, broadcast, kio, origin};

/// `(viewers, broadcasts)` shapes: viewers spread round-robin over the broadcasts.
const SHAPES: [(usize, usize); 4] = [(100, 1), (1_000, 1), (1_000, 100), (10_000, 100)];

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicIsize = AtomicIsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

fn count(bytes: isize) {
	if COUNTING.load(Ordering::Relaxed) {
		LIVE.fetch_add(bytes, Ordering::Relaxed);
	}
}

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		count(layout.size() as isize);
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		count(layout.size() as isize);
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		count(new_size as isize - layout.size() as isize);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		count(-(layout.size() as isize));
		unsafe { System.dealloc(ptr, layout) }
	}
}

/// An origin publishing `room/<i>` broadcasts, each with one track holding one
/// group, and its driver polled in place.
struct Room {
	producer: origin::Producer,
	driver: origin::Driver,
	broadcasts: usize,
	_sources: Vec<(broadcast::Producer, moq_net::track::Producer)>,
}

impl Room {
	fn new(broadcasts: usize) -> Self {
		let (producer, driver) = origin::Producer::new(origin::Config::default());
		let _sources = (0..broadcasts)
			.map(|i| {
				let broadcast = producer.publish(format!("room/{i}"), origin::Route::default()).unwrap();
				let track = broadcast.create_track("video", None).unwrap();
				let mut group = track.append_group().unwrap();
				group.write_frame(Timestamp::ZERO, b"frame".as_ref()).unwrap();
				group.finish().unwrap();
				(broadcast, track)
			})
			.collect();
		Self {
			producer,
			driver,
			broadcasts,
			_sources,
		}
	}

	fn poll(&mut self) {
		self.driver
			.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
			.unwrap();
	}

	/// Run `pending` to completion, polling the driver until it is.
	fn drive<T>(&mut self, pending: impl Future<Output = T>) -> T {
		let mut pending = std::pin::pin!(pending);
		loop {
			if let Some(output) = pending.as_mut().now_or_never() {
				return output;
			}
			self.poll();
		}
	}

	/// Viewer `n` joins and reads the latest group of its broadcast.
	fn join(&mut self, n: usize) -> moq_net::track::Subscriber {
		let session = moq_net::fuzz::excluding(self.producer.consume(), Hop::random());
		let path = format!("room/{}", n % self.broadcasts);
		let resolved = self.drive(session.request_broadcast(path, None)).unwrap();
		let mut subscription = self.drive(resolved.track("video").unwrap().subscribe(None)).unwrap();
		self.drive(subscription.recv_group())
			.unwrap()
			.expect("the latest group");
		subscription
	}

	/// A viewer leaves, and the front sees it go.
	fn leave(&mut self, subscription: moq_net::track::Subscriber) {
		drop(subscription);
		self.poll();
	}
}

fn id(viewers: usize, broadcasts: usize) -> BenchmarkId {
	BenchmarkId::from_parameter(format!("{viewers}v_{broadcasts}b"))
}

/// One more viewer joining and leaving while `viewers` watch.
fn bench_join(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin/viewer_join");
	for (viewers, broadcasts) in SHAPES {
		group.bench_function(id(viewers, broadcasts), |b| {
			let mut room = Room::new(broadcasts);
			let _watching: Vec<_> = (0..viewers).map(|n| room.join(n)).collect();
			let mut next = 0;
			b.iter(|| {
				let subscription = room.join(next);
				next += 1;
				room.leave(subscription);
			});
		});
	}
	group.finish();
}

/// A route change covering every broadcast after `viewers` came and went: every
/// front beneath it wakes to re-select, so the cost follows the fronts the viewers
/// left behind. First prints the heap each departed viewer left behind.
fn bench_churn(c: &mut Criterion) {
	eprintln!("origin heap retained per departed viewer:");
	for (viewers, broadcasts) in SHAPES {
		let mut room = Room::new(broadcasts);
		// Warm up, so no viewer is billed for the front its broadcast keeps anyway.
		for n in 0..broadcasts {
			let subscription = room.join(n);
			room.leave(subscription);
		}
		LIVE.store(0, Ordering::Relaxed);
		COUNTING.store(true, Ordering::Relaxed);
		for n in 0..viewers {
			let subscription = room.join(n);
			room.leave(subscription);
		}
		COUNTING.store(false, Ordering::Relaxed);
		let retained = LIVE.load(Ordering::Relaxed) as f64 / viewers as f64;
		eprintln!("  {viewers:>6} viewers, {broadcasts:>3} broadcasts: {retained:>8.1} bytes");
	}

	let mut group = c.benchmark_group("origin/viewer_churn");
	for (viewers, broadcasts) in SHAPES {
		group.bench_function(id(viewers, broadcasts), |b| {
			let mut room = Room::new(broadcasts);
			for n in 0..viewers {
				let subscription = room.join(n);
				room.leave(subscription);
			}
			b.iter(|| {
				let covering = room.producer.dynamic("room", origin::Route::default()).unwrap();
				room.poll();
				drop(covering);
				room.poll();
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_join, bench_churn);
criterion_main!(benches);
