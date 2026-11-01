//! The end check of a received track whose declared end still waits on its tail.
//!
//! `track_pending_tail` holds a reader at a hole below a pending end on every track, each
//! track caching `groups` groups above the hole, and re-polls one of those readers. The
//! check is judged by the track's record of arrivals, so a cost that grows with the groups
//! cached or the tracks held shows up as a slope over either axis.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench tail`.

use std::{hint::black_box, task::Poll, time::Duration};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{broadcast, group, kio, track};

/// Tracks each holding a reader at the tail.
const TRACKS: [usize; 3] = [1, 32, 1024];

/// Groups each track caches above the hole.
const GROUPS: [usize; 3] = [1, 32, 1024];

/// Readers held at a hole below a pending end, one per track.
struct Held {
	_broadcast: broadcast::Producer,
	_tracks: Vec<track::Producer>,
	readers: Vec<track::Subscriber>,
}

impl Held {
	fn new(tracks: usize, groups: usize) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let waiter = kio::Waiter::noop();
		let mut readers = Vec::with_capacity(tracks);
		let tracks = (0..tracks)
			.map(|i| {
				let mut track = broadcast.create_track(format!("bench{i}"), None).unwrap();
				// A budget wide enough that no cached group is skipped as stale.
				let mut reader =
					track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
				track.start_at(0).unwrap();
				let end = track::Position::group(groups as u64 + 1);
				moq_net::fuzz::received(&track);
				track.finish_at(end, None).unwrap();
				// Group 0 never arrives: the hole that holds the reader.
				for sequence in 1..=groups as u64 {
					track.create_group(group::Info { sequence }).unwrap().finish().unwrap();
				}
				for _ in 0..groups {
					assert!(matches!(reader.poll_recv_group(&waiter), Poll::Ready(Ok(Some(_)))));
				}
				assert!(reader.poll_recv_group(&waiter).is_pending(), "held at the hole");
				readers.push(reader);
				track
			})
			.collect();
		Self {
			_broadcast: broadcast,
			_tracks: tracks,
			readers,
		}
	}
}

fn bench_pending_tail(c: &mut Criterion) {
	let mut group = c.benchmark_group("track_pending_tail");
	group.throughput(Throughput::Elements(1));
	for tracks in TRACKS {
		for groups in GROUPS {
			let id = format!("{tracks}x{groups}");
			group.bench_function(BenchmarkId::new("repoll", &id), |b| {
				let mut held = Held::new(tracks, groups);
				let waiter = kio::Waiter::noop();
				b.iter(|| assert!(black_box(held.readers[0].poll_recv_group(&waiter)).is_pending()));
			});
		}
	}
	group.finish();
}

criterion_group!(benches, bench_pending_tail);
criterion_main!(benches);
