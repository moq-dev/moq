//! A join judged by the track's refresh ratio, and a reader's refresh judged by its
//! interval, with the demand each raises: swept over tracks and subscribers per track. The
//! judgment reads only the asked track's newest group, so a cost that grows with the other
//! tracks or the track's audience shows up as a slope. The group answering each request is
//! written outside the timed section.
//!
//! Run with `cargo bench -p moq-net --bench refresh`.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{broadcast, track};

const TRACKS: [usize; 3] = [1, 100, 1_000];
const SUBSCRIBERS: [usize; 3] = [1, 10, 100];

/// Append group `key`, whose delta outweighs its first frame.
fn refreshable(track: &track::Producer, key: u64) {
	let start = moq_net::Timestamp::unscaled(key);
	let mut group = track.append_group(start).unwrap();
	group.write_frame(start, Bytes::from_static(b"key")).unwrap();
	group.write_frame(start, Bytes::from_static(b"delta")).unwrap();
	group.finish().unwrap();
}

fn join(c: &mut Criterion) {
	let mut group = c.benchmark_group("refresh/join");
	for tracks in TRACKS {
		for subscribers in SUBSCRIBERS {
			let broadcast = broadcast::Info::default().produce();
			let info = track::Info::default().with_max_age(None).with_refresh(moq_net::track::Refresh::default().with_ratio(1));
			let producers: Vec<_> = (0..tracks)
				.map(|i| broadcast.create_track(format!("bench{i}"), info.clone()).unwrap())
				.collect();
			let _audience: Vec<Vec<_>> = producers
				.iter()
				.map(|track| (0..subscribers).map(|_| track.subscribe(None)).collect())
				.collect();
			let track = &producers[tracks / 2];
			let demand = track.demand();
			refreshable(track, 0);
			let mut latest = 0;
			group.bench_function(BenchmarkId::new(format!("{tracks}x"), subscribers), |b| {
				b.iter_custom(|iterations| {
					let mut elapsed = Duration::ZERO;
					for _ in 0..iterations {
						let start = Instant::now();
						let joined = track.subscribe(None);
						let asked = demand.poll_refresh(&kio::Waiter::noop()).is_ready();
						elapsed += start.elapsed();
						assert!(asked);
						drop(joined);
						latest += 1;
						refreshable(track, latest);
					}
					elapsed
				})
			});
		}
	}
	group.finish();
}

fn refresh(c: &mut Criterion) {
	let mut group = c.benchmark_group("refresh/request");
	for tracks in TRACKS {
		for subscribers in SUBSCRIBERS {
			let broadcast = broadcast::Info::default().produce();
			let refresh = track::Refresh::default().with_interval(Duration::ZERO);
			let info = track::Info::default().with_max_age(None).with_refresh(refresh);
			let producers: Vec<_> = (0..tracks)
				.map(|i| broadcast.create_track(format!("bench{i}"), info.clone()).unwrap())
				.collect();
			let audience: Vec<Vec<_>> = producers
				.iter()
				.map(|track| (0..subscribers).map(|_| track.subscribe(None)).collect())
				.collect();
			let asking = &audience[tracks / 2];
			let track = &producers[tracks / 2];
			let demand = track.demand();
			refreshable(track, 0);
			let mut latest = 0;
			group.bench_function(BenchmarkId::new(format!("{tracks}x"), subscribers), |b| {
				b.iter_custom(|iterations| {
					let mut elapsed = Duration::ZERO;
					for _ in 0..iterations {
						let start = Instant::now();
						// Every reader asks; all but the first coalesce.
						for reader in asking {
							reader.refresh();
						}
						let asked = demand.poll_refresh(&kio::Waiter::noop()).is_ready();
						elapsed += start.elapsed();
						assert!(asked);
						latest += 1;
						refreshable(track, latest);
					}
					elapsed
				})
			});
		}
	}
	group.finish();
}

criterion_group!(benches, join, refresh);
criterion_main!(benches);
