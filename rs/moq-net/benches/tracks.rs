//! What a broadcast's track listing costs as it fans out.
//!
//! - change: a track is created and dropped on one broadcast, and every listing of it
//!   reads both events. Swept over the listings and the tracks it already lists, and
//!   over idle broadcasts with listings of their own, so a cost that grows with the
//!   table rather than the touched broadcast shows up as a slope.
//! - list: a new listing reads a broadcast's tracks through SYNCED, swept the same way.
//!
//! Run with `cargo bench -p moq-net --bench tracks`.

use std::task::Poll;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::broadcast::{self, Tracks, TracksEvent};

/// `(listings, tracks, broadcasts)` shapes: listings and tracks per broadcast, and the
/// broadcasts beside the touched one.
const SHAPES: [(usize, usize, usize); 6] = [
	(1, 10, 1),
	(1, 1_000, 1),
	(100, 10, 1),
	(100, 1_000, 1),
	(10, 10, 1_000),
	(10, 10, 1),
];

/// Broadcasts with their tracks and listings, every listing drained to SYNCED.
struct Table {
	broadcasts: Vec<broadcast::Producer>,
	_tracks: Vec<moq_net::track::Producer>,
	listings: Vec<Vec<Tracks>>,
}

fn next(tracks: &mut Tracks) -> Option<TracksEvent> {
	match tracks.poll_next(&kio::Waiter::noop()) {
		Poll::Ready(event) => event.expect("listing failed"),
		Poll::Pending => None,
	}
}

fn drain(tracks: &mut Tracks) -> usize {
	let mut events = 0;
	while next(tracks).is_some() {
		events += 1;
	}
	events
}

fn table(listings: usize, tracks: usize, broadcasts: usize) -> Table {
	let mut table = Table {
		broadcasts: Vec::new(),
		_tracks: Vec::new(),
		listings: Vec::new(),
	};
	for _ in 0..broadcasts {
		let broadcast = broadcast::Info::new().produce();
		for i in 0..tracks {
			table
				._tracks
				.push(broadcast.create_track(format!("t{i}"), None).unwrap());
		}
		let consumer = broadcast.consume();
		let mut readers: Vec<Tracks> = (0..listings).map(|_| consumer.tracks().unwrap()).collect();
		for reader in &mut readers {
			assert_eq!(drain(reader), tracks + 1);
		}
		table.broadcasts.push(broadcast);
		table.listings.push(readers);
	}
	table
}

fn bench_change(c: &mut Criterion) {
	let mut group = c.benchmark_group("tracks/change");
	for (listings, tracks, broadcasts) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{listings}l_{tracks}t_{broadcasts}b"));
		group.bench_function(id, |b| {
			let mut table = table(listings, tracks, broadcasts);
			let broadcast = table.broadcasts[0].clone();
			let readers = &mut table.listings[0];
			b.iter(|| {
				let track = broadcast.create_track("new", None).unwrap();
				drop(track);
				for reader in readers.iter_mut() {
					// The unread add and removal cancel; read the next pair as it lands.
					assert_eq!(drain(reader), 0);
				}
				let track = broadcast.create_track("new", None).unwrap();
				for reader in readers.iter_mut() {
					assert_eq!(drain(reader), 1);
				}
				drop(track);
				for reader in readers.iter_mut() {
					assert_eq!(drain(reader), 1);
				}
			});
		});
	}
	group.finish();
}

fn bench_list(c: &mut Criterion) {
	let mut group = c.benchmark_group("tracks/list");
	for (listings, tracks, broadcasts) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{listings}l_{tracks}t_{broadcasts}b"));
		group.bench_function(id, |b| {
			let table = table(listings, tracks, broadcasts);
			let consumer = table.broadcasts[0].consume();
			b.iter(|| {
				let mut reader = consumer.tracks().unwrap();
				assert_eq!(drain(&mut reader), tracks + 1);
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_change, bench_list);
criterion_main!(benches);
