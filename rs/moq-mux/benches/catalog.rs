//! Catalog churn from a rising flush estimate, swept over tracks x subscribers.
//!
//! Every track's jitter rises 1 ms per frame for [`RISES`] frames inside one rate window, then one
//! more frame lands after the window. Each catalog a subscriber receives makes a player update its
//! subscription on every track, so the work fans out as `publishes x tracks x subscribers`. The
//! routine asserts the rate limit holds that to two publishes (the leading and trailing edge)
//! whatever the shape; without it the count would be `RISES x tracks x subscribers`.
//!
//! The rate window runs on tokio's paused clock, so the counts are deterministic.
//!
//! Run with `cargo bench -p moq-mux --bench catalog`.

use std::hint::black_box;
use std::task::Poll;
use std::time::{Duration, Instant};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_mux::catalog::hang::Container;
use moq_mux::container::Kind;
use moq_net::Timestamp;

/// Tracks on the broadcast: a single rendition, a small ladder, a large ladder.
const TRACKS: [usize; 3] = [1, 4, 16];

/// Catalog subscribers: a direct viewer, a small room, a large room.
const SUBSCRIBERS: [usize; 3] = [1, 4, 16];

/// Estimate rises inside one window.
const RISES: u64 = 1000;

struct Fanout {
	_broadcast: moq_net::broadcast::Producer,
	_catalog: moq_mux::catalog::Producer,
	tracks: Vec<moq_mux::container::Producer<Container, hang::catalog::VideoConfig>>,
	subscribers: Vec<moq_mux::catalog::hang::Consumer>,
	waiter: kio::Waiter,
	anchor: Instant,
}

impl Fanout {
	fn new(tracks: usize, subscribers: usize) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let tracks = (0..tracks)
			.map(|index| {
				let info = catalog.track_info(hang::catalog::PRIORITY.video);
				let track = broadcast.create_track(format!("v{index}"), info).unwrap();
				let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
				catalog.video(track, Container::Legacy(Kind::Video), config).unwrap()
			})
			.collect();
		let mut fanout = Self {
			subscribers: (0..subscribers).map(|_| catalog.consume().unwrap()).collect(),
			_broadcast: broadcast,
			_catalog: catalog,
			tracks,
			waiter: kio::Waiter::noop(),
			anchor: Instant::now(),
		};
		// The initial track set is configuration, not estimate traffic.
		fanout.receive();
		fanout
	}

	/// Every frame lands `frame` ms late, after an on-time first frame, so jitter rises 1 ms each.
	fn flush(&mut self, frame: u64) {
		let timestamp = Timestamp::from_millis(frame).unwrap();
		let now = self.anchor + Duration::from_millis(2 * frame);
		for track in &mut self.tracks {
			track.flush(timestamp, now).unwrap();
		}
	}

	/// Drain each subscriber, returning the catalogs subscriber zero received and the subscription
	/// updates every subscriber's player would send for them.
	fn receive(&mut self) -> (usize, usize) {
		let (mut publishes, mut updates) = (0, 0);
		for (index, subscriber) in self.subscribers.iter_mut().enumerate() {
			while let Poll::Ready(catalog) = subscriber.poll_next(&self.waiter) {
				let catalog = catalog.unwrap().unwrap();
				if index == 0 {
					publishes += 1;
				}
				// One subscription update per track, as a player re-reads each rendition's jitter.
				for rendition in catalog.video.renditions.values() {
					black_box(rendition.jitter);
					updates += 1;
				}
			}
		}
		(publishes, updates)
	}

	async fn run(mut self) {
		let (mut publishes, mut updates) = (0, 0);
		let mut tally = |(p, u): (usize, usize)| {
			publishes += p;
			updates += u;
		};
		for frame in 0..=RISES {
			self.flush(frame);
			tally(self.receive());
		}
		tokio::time::advance(Duration::from_secs(1)).await;
		self.flush(RISES);
		tally(self.receive());

		let shape = self.tracks.len() * self.subscribers.len();
		assert_eq!(
			(publishes, updates),
			(2, 2 * shape),
			"estimate churn regression with {} tracks x {} subscribers",
			self.tracks.len(),
			self.subscribers.len()
		);
	}
}

fn bench_estimate_fanout(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();
	let mut group = c.benchmark_group("catalog_estimate_fanout");
	for tracks in TRACKS {
		for subscribers in SUBSCRIBERS {
			group.throughput(Throughput::Elements(RISES * tracks as u64));
			group.bench_with_input(
				BenchmarkId::new(format!("tracks_{tracks}"), subscribers),
				&(tracks, subscribers),
				|b, &(tracks, subscribers)| {
					b.iter_batched(
						|| Fanout::new(tracks, subscribers),
						|fanout| runtime.block_on(fanout.run()),
						BatchSize::SmallInput,
					);
				},
			);
		}
	}
	group.finish();
}

criterion_group!(benches, bench_estimate_fanout);
criterion_main!(benches);
