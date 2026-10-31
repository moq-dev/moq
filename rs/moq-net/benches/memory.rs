//! Frame throughput over the in-memory transport: one publisher session sending to one
//! subscriber session, swept over the streams in flight (one group per track) and the
//! frame size.
//!
//! Each round writes one group of [`FRAMES`] frames on every track, then reads them all
//! back. The payload crosses as the written allocation, so the time per byte should
//! fall as frames grow, and `MEMORY_ALLOCS=1` prints the allocations and bytes allocated
//! per frame instead of timing: a copy would show as bytes per frame growing with the
//! frame size.
//!
//! Everything runs on one current-thread runtime, so the only work measured is the
//! protocol, model, and transport code, never scheduling across threads.
//!
//! Run with `cargo bench -p moq-net --bench memory`.

#![expect(
	clippy::disallowed_methods,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Client, Hop, Server, Timestamp, Version, cache, origin, track, transport::memory};

const VERSION: &str = "moq-lite-06";

/// Frames per group, so per-frame and per-group costs both appear.
const FRAMES: usize = 4;

const SIZES: [usize; 4] = [64, 16 * 1024, 1024 * 1024, 4 * 1024 * 1024];
const STREAMS: [usize; 3] = [1, 16, 128];

/// The most payload one round may write across every stream, so the largest frames skip
/// the widest sweep rather than hold gigabytes.
const ROUND_BYTES: usize = 512 * 1024 * 1024;

/// Every `(streams, size)` pair to measure.
fn cases() -> impl Iterator<Item = (usize, usize)> {
	STREAMS
		.into_iter()
		.flat_map(|streams| SIZES.into_iter().map(move |size| (streams, size)))
		.filter(|(streams, size)| streams * FRAMES * size <= ROUND_BYTES)
}

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

impl Counter {
	fn count(size: usize) {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
			ALLOCATED.fetch_add(size, Ordering::Relaxed);
		}
	}
}

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		Self::count(layout.size());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		Self::count(layout.size());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		Self::count(new_size);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

fn now() -> moq_net::time::Instant {
	moq_clock::now()
}

/// One publisher and one subscriber session over an in-memory pair.
struct Link {
	tracks: Vec<track::Producer>,
	subscribers: Vec<track::Subscriber>,
	payload: Bytes,
	_broadcast: origin::Publication,
	_sessions: (moq_net::Session, moq_net::Session),
	_origins: (origin::Producer, origin::Producer),
}

impl Link {
	async fn new(streams: usize, size: usize) -> Self {
		let version: Version = VERSION.parse().unwrap();
		let origin = |hop: u64| {
			let mut config = origin::Config::new(Hop::new(hop).unwrap());
			// Hold one round's groups, with room for the handover, and no more.
			config.pool =
				cache::Pool::new(cache::Config::default().with_capacity((4 * streams * FRAMES * size) as u64));
			let (producer, driver) = origin::Producer::new(config);
			tokio::spawn(moq_net::time::run(driver));
			producer
		};
		let publisher = origin(1);
		let subscriber = origin(2);

		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client, server) = memory::pair(config);
		let client_config = Client::new()
			.with_versions(version.into())
			.with_publisher(publisher.consume());
		let server_config = Server::new()
			.with_versions(version.into())
			.with_subscriber(subscriber.clone());
		// Each driver runs as soon as its handshake resolves, since the server's waits on
		// the client's driver.
		let sessions = futures::join!(
			async {
				let (session, driver) = client_config.connect(now(), client).await.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			},
			async {
				let (session, driver) = server_config.accept(now(), server).await.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			}
		);

		let broadcast = publisher.create_broadcast("bench").unwrap();
		let info = track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let tracks: Vec<_> = (0..streams)
			.map(|index| broadcast.create_track(format!("{index}"), info.clone()).unwrap())
			.collect();
		broadcast.announce(Default::default()).unwrap();

		let consumer = subscriber.consume();
		let remote = consumer.follow("bench", None).unwrap().available().await.unwrap();
		let mut subscribers = Vec::new();
		for index in 0..streams {
			let track = remote.track(&format!("{index}")).unwrap();
			subscribers.push(track.subscribe(None).await.unwrap());
		}

		let mut link = Self {
			tracks,
			subscribers,
			payload: Bytes::from(vec![0; size]),
			_broadcast: broadcast,
			_sessions: sessions,
			_origins: (publisher, subscriber),
		};
		// Warm every stream so the timed rounds skip first-group setup.
		for _ in 0..4 {
			link.round().await;
		}
		link
	}

	/// Write a group on every track, then read each back, returning the payload bytes
	/// read so skipped work can't look like a speedup.
	async fn round(&mut self) -> usize {
		for track in &self.tracks {
			let mut group = track.append_group().unwrap();
			for _ in 0..FRAMES {
				group.write_frame(Timestamp::ZERO, self.payload.clone()).unwrap();
			}
			group.finish().unwrap();
		}

		let mut bytes = 0;
		for subscriber in &mut self.subscribers {
			let mut group = subscriber.recv_group().await.unwrap().expect("track ended");
			while let Some(frame) = group.read_frame().await.unwrap() {
				bytes += frame.payload.len();
			}
		}
		bytes
	}

	async fn measure(&mut self, iters: u64) -> Duration {
		let expected = self.tracks.len() * FRAMES * self.payload.len();
		let start = Instant::now();
		for _ in 0..iters {
			assert_eq!(self.round().await, expected);
		}
		start.elapsed()
	}
}

/// Print allocations and bytes allocated per frame instead of timing.
fn allocations() {
	const ROUNDS: usize = 64;

	let rt = runtime();
	for (streams, size) in cases() {
		let mut link = rt.block_on(Link::new(streams, size));
		ALLOCATIONS.store(0, Ordering::Relaxed);
		ALLOCATED.store(0, Ordering::Relaxed);
		COUNTING.store(true, Ordering::Relaxed);
		rt.block_on(link.measure(ROUNDS as u64));
		COUNTING.store(false, Ordering::Relaxed);

		let frames = (ROUNDS * streams * FRAMES) as f64;
		let allocations = ALLOCATIONS.load(Ordering::Relaxed) as f64 / frames;
		let bytes = ALLOCATED.load(Ordering::Relaxed) as f64 / frames;
		println!("streams={streams}/size={size}: {allocations:.1} allocations, {bytes:.0} bytes allocated per frame");
	}
}

fn bench(c: &mut Criterion) {
	if std::env::var_os("MEMORY_ALLOCS").is_some() {
		allocations();
		return;
	}

	let rt = runtime();
	let mut group = c.benchmark_group("memory");
	for (streams, size) in cases() {
		group.throughput(Throughput::Bytes((streams * FRAMES * size) as u64));
		let mut link = None;
		group.bench_function(
			BenchmarkId::from_parameter(format!("streams={streams}/size={size}")),
			|b| {
				let link = link.get_or_insert_with(|| rt.block_on(Link::new(streams, size)));
				b.iter_custom(|iters| rt.block_on(link.measure(iters)))
			},
		);
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
