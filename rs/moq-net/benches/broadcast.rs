//! Track-name watchers on a broadcast, swept over the tracks it holds and the
//! watchers attached to it.
//!
//! `broadcast/tracks_create` creates one track and drains its name from every
//! watcher: the fan-out is inherent, so the cost should grow with watchers and not
//! with the tracks already held. `broadcast/tracks_watch` opens a watcher, drains
//! the snapshot, and drops it: the cost should grow with tracks and not with the
//! other watchers.
//!
//! Run with `cargo bench -p moq-net --bench broadcast`.

use std::task::Poll;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{broadcast, kio, track};

const TRACKS: [usize; 3] = [1, 64, 1_024];
const WATCHERS: [usize; 3] = [1, 64, 1_024];

/// A broadcast holding `tracks` tracks, watched by `watchers` drained watchers.
fn setup(tracks: usize, watchers: usize) -> (broadcast::Producer, Vec<track::Producer>, Vec<broadcast::Tracks>) {
	let producer = broadcast::Info::new().produce();
	let held = (0..tracks)
		.map(|i| producer.create_track(format!("held/{i}"), None).unwrap())
		.collect();
	let mut watching: Vec<_> = (0..watchers).map(|_| producer.tracks()).collect();
	for watcher in &mut watching {
		drain(watcher);
	}
	(producer, held, watching)
}

fn drain(watcher: &mut broadcast::Tracks) -> usize {
	let mut count = 0;
	while let Poll::Ready(Some(_)) = watcher.poll_next(&kio::Waiter::noop()) {
		count += 1;
	}
	count
}

fn bench_create(c: &mut Criterion) {
	let mut group = c.benchmark_group("broadcast/tracks_create");
	for tracks in TRACKS {
		for watchers in WATCHERS {
			let id = BenchmarkId::from_parameter(format!("{tracks}t_{watchers}w"));
			group.bench_function(id, |b| {
				let (producer, _held, mut watching) = setup(tracks, watchers);
				let mut next = 0u64;
				b.iter(|| {
					next += 1;
					let track = producer.create_track(format!("new/{next}"), None).unwrap();
					for watcher in &mut watching {
						assert_eq!(drain(watcher), 1);
					}
					drop(track);
				});
			});
		}
	}
	group.finish();
}

fn bench_watch(c: &mut Criterion) {
	let mut group = c.benchmark_group("broadcast/tracks_watch");
	for tracks in TRACKS {
		for watchers in WATCHERS {
			let id = BenchmarkId::from_parameter(format!("{tracks}t_{watchers}w"));
			group.bench_function(id, |b| {
				let (producer, _held, _watching) = setup(tracks, watchers);
				b.iter(|| {
					let mut watcher = producer.tracks();
					assert_eq!(drain(&mut watcher), tracks);
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, bench_create, bench_watch);
criterion_main!(benches);
