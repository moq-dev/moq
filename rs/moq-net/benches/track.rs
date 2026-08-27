//! Producer-to-subscriber fanout benchmarks for the track model.
//!
//! The group benchmarks isolate per-frame storage. These benchmarks include the
//! track cache and subscription cursors around that storage: append a complete
//! group, notify every subscriber, and hand the cached group to each cursor.
//!
//! Run with `cargo bench -p moq-net --bench track`.

use std::hint::black_box;
use std::task::Poll;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Timestamp, broadcast, track};

/// Fanout sizes spanning a direct viewer, a small room, and a large room.
const FANOUT: [usize; 4] = [1, 8, 64, 512];

/// Small shared payload so the benchmark measures model overhead, not allocation.
const PAYLOAD: usize = 64;

/// Keeps the ownership chain alive around the track and its subscribers.
struct Fanout {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	subscribers: Vec<track::Subscriber>,
	waiter: kio::Waiter,
	payload: Bytes,
}

impl Fanout {
	fn new(subscribers: usize) -> Self {
		let mut broadcast = broadcast::Producer::new(broadcast::Info::default());
		let track = broadcast.create_track("bench", None).unwrap();
		let subscribers = (0..subscribers).map(|_| track.subscribe(None)).collect();

		Self {
			_broadcast: broadcast,
			track,
			subscribers,
			waiter: kio::Waiter::noop(),
			payload: Bytes::from(vec![0; PAYLOAD]),
		}
	}

	/// Append one finished group and deliver its handle to every subscriber.
	fn cycle(&mut self) {
		let mut group = self.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, self.payload.clone()).unwrap();
		group.finish().unwrap();

		for subscriber in &mut self.subscribers {
			let group = match subscriber.poll_recv_group(&self.waiter) {
				Poll::Ready(Ok(Some(group))) => group,
				_ => unreachable!("a completed group must be ready"),
			};
			black_box(group);
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

criterion_group!(benches, bench_fanout);
criterion_main!(benches);
