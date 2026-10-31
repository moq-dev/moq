//! Catalog churn from a rising estimate, swept over tracks x subscribers.
//!
//! Every track's jitter rises 1 ms per frame for [`RISES`] frames inside one rate window, then one
//! more frame lands after the window. Each catalog a subscriber receives makes a player update its
//! subscription on every track, so the work fans out as `publishes x tracks x subscribers`. The
//! hang case asserts the rate limit holds that to two publishes (the leading and trailing edge)
//! whatever the shape; without it the count would be `RISES x tracks x subscribers`.
//!
//! The MSF reference case declares renditions in distinct sibling namespaces and converts the
//! initial catalog back for every subscriber. MSF keeps the estimates it first declared, so this
//! case asserts that measured rises cause no further catalogs or subscription updates.
//! The rate window runs on tokio's paused clock, so the counts are deterministic.
//!
//! The teardown case drops a reserved importer whose declared tracks never got a config, as at the
//! end of malformed input. Each drop releases one reservation, so a per-release scan of the
//! remaining tracks would show up as a quadratic slope over the track count.
//!
//! The write order case interleaves frames across every track of one catalog, each measuring its
//! `delay` against the others, so a per-frame scan of the tracks would show up as a slope.
//!
//! Run with `cargo bench -p moq-mux --bench catalog`.

use std::hint::black_box;
use std::task::Poll;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_mux::catalog::Stream;
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
	subscribers: Vec<moq_mux::catalog::Consumer>,
	waiter: kio::Waiter,
}

impl Fanout {
	async fn new(tracks: usize, subscribers: usize, references: bool) -> Self {
		let mut info = moq_net::broadcast::Info::new();
		info.path = moq_net::Path::from_static("room/catalog");
		let mut broadcast = info.produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let tracks = (0..tracks)
			.map(|index| {
				let info = catalog.track_info(hang::catalog::PRIORITY.video);
				let track = broadcast.create_track(format!("v{index}"), info).unwrap();
				let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
				if references {
					config.broadcast = Some(moq_net::path::Relative::new(&format!("./source-{index}")).unwrap());
				}
				catalog.video(track, Container::Legacy(Kind::Video), config).unwrap()
			})
			.collect();
		let mut readers = Vec::new();
		for _ in 0..subscribers {
			let reader = if references {
				moq_mux::catalog::Consumer::new(&broadcast.consume(), moq_mux::catalog::CatalogFormat::Msf)
					.await
					.unwrap()
			} else {
				moq_mux::catalog::Consumer::from(catalog.consume().unwrap())
			};
			readers.push(reader);
		}
		let mut fanout = Self {
			subscribers: readers,
			_broadcast: broadcast,
			_catalog: catalog,
			tracks,
			waiter: kio::Waiter::noop(),
		};
		// The initial track set is configuration, not estimate traffic.
		assert_eq!(fanout.receive(), (1, fanout.tracks.len() * subscribers));
		fanout
	}

	/// Every frame is reordered by `frame` ms, after an in-order first frame, so jitter rises 1 ms
	/// each. The cut publishes it.
	fn rise(&mut self, frame: u64) {
		let delay = Timestamp::from_millis(frame).unwrap();
		for track in &mut self.tracks {
			track.reorder(delay);
			track.cut(None).unwrap();
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
					if let Some(reference) = &rendition.broadcast {
						assert!(
							moq_net::Path::from_static("room/catalog")
								.try_resolve(reference)
								.unwrap()
								.as_str()
								.starts_with("room/source-")
						);
					}
					updates += 1;
				}
			}
		}
		(publishes, updates)
	}

	async fn run(mut self, expected: usize) {
		let (mut publishes, mut updates) = (0, 0);
		let mut tally = |(p, u): (usize, usize)| {
			publishes += p;
			updates += u;
		};
		for frame in 0..=RISES {
			self.rise(frame);
			tally(self.receive());
		}
		tokio::time::advance(Duration::from_secs(1)).await;
		self.rise(RISES);
		tally(self.receive());

		let shape = self.tracks.len() * self.subscribers.len();
		assert_eq!(
			(publishes, updates),
			(expected, expected * shape),
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
	for (mode, references) in [("hang", false), ("msf_references", true)] {
		let expected = if references { 0 } else { 2 };
		for tracks in TRACKS {
			for subscribers in SUBSCRIBERS {
				group.throughput(Throughput::Elements(RISES * tracks as u64));
				group.bench_with_input(
					BenchmarkId::new(format!("{mode}/tracks_{tracks}"), subscribers),
					&(tracks, subscribers),
					|b, &(tracks, subscribers)| {
						b.iter_batched(
							|| runtime.block_on(Fanout::new(tracks, subscribers, references)),
							|fanout| runtime.block_on(fanout.run(expected)),
							BatchSize::SmallInput,
						);
					},
				);
			}
		}
	}
	group.finish();
}

/// Declared tracks on an importer that ends before configuring any.
const UNRESOLVED: [usize; 3] = [1, 64, 1024];

struct Unresolved {
	_broadcast: moq_net::broadcast::Producer,
	_catalog: moq_mux::catalog::Producer,
	tracks: Vec<moq_mux::container::Producer<Container, hang::catalog::VideoConfig>>,
}

impl Unresolved {
	fn new(tracks: usize) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let reserved = catalog.reserve();
		let tracks = (0..tracks)
			.map(|index| {
				let info = reserved.track_info(hang::catalog::PRIORITY.video);
				let track = broadcast.create_track(format!("v{index}"), info).unwrap();
				reserved.video(track, Container::Legacy(Kind::Video), None).unwrap()
			})
			.collect();
		Self {
			_broadcast: broadcast,
			_catalog: catalog,
			tracks,
		}
	}
}

fn bench_unresolved_teardown(c: &mut Criterion) {
	let mut group = c.benchmark_group("catalog_unresolved_teardown");
	for tracks in UNRESOLVED {
		group.throughput(Throughput::Elements(tracks as u64));
		group.bench_with_input(BenchmarkId::from_parameter(tracks), &tracks, |b, &tracks| {
			b.iter_batched(
				|| Unresolved::new(tracks),
				|mut unresolved| unresolved.tracks.clear(),
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

/// Tracks interleaving frames on one catalog.
const ORDERED: [usize; 3] = [1, 16, 64];

/// Frames each track writes per iteration.
const FRAMES: u64 = 100;

fn bench_write_order(c: &mut Criterion) {
	let mut group = c.benchmark_group("catalog_write_order");
	for tracks in ORDERED {
		group.throughput(Throughput::Elements(FRAMES * tracks as u64));
		group.bench_with_input(BenchmarkId::from_parameter(tracks), &tracks, |b, &tracks| {
			b.iter_batched(
				|| {
					let mut broadcast = moq_net::broadcast::Info::new().produce();
					let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
					let producers: Vec<_> = (0..tracks)
						.map(|index| {
							let info = catalog.track_info(hang::catalog::PRIORITY.video);
							let track = broadcast.create_track(format!("v{index}"), info).unwrap();
							let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
							catalog.video(track, Container::Legacy(Kind::Video), config).unwrap()
						})
						.collect();
					(broadcast, catalog, producers)
				},
				|(_broadcast, _catalog, mut producers)| {
					for frame in 0..FRAMES {
						// Each track trails the one before it by a millisecond, so every write measures.
						for (index, producer) in producers.iter_mut().enumerate() {
							let timestamp = Duration::from_millis(frame * 40 + 1_000)
								.saturating_sub(Duration::from_millis(index as u64));
							producer
								.write(moq_mux::container::Frame {
									timestamp: Timestamp::from_micros(timestamp.as_micros() as u64).unwrap(),
									payload: bytes::Bytes::from_static(&[0; 64]),
									keyframe: frame == 0,
									duration: None,
								})
								.unwrap();
						}
					}
				},
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_estimate_fanout,
	bench_unresolved_teardown,
	bench_write_order
);
criterion_main!(benches);
