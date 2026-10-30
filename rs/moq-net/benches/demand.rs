//! What a broadcast's demand costs as it fans out.
//!
//! - edge: a reader subscribes to one track through the broadcast and leaves, and every
//!   [`broadcast::Demand`] handle reads both edges. Swept over the tracks the broadcast
//!   holds and the handles watching it, so a cost that grows with the table rather than
//!   the touched track shows up as a slope.
//! - churn: a short-lived track comes and goes while handles that never poll watch the
//!   broadcast, the insert each handle's inbox absorbs. Swept the same way, with the live
//!   tracks one short of a power of two so they fill an inbox's capacity but one slot.
//! - lookup: a subscriber looks up and subscribes to a track the broadcast already serves,
//!   beside the readers already on it, while a parked [`broadcast::Demand`] handle counts
//!   its wakes. Swept over the tracks the broadcast holds and the readers on the track; a
//!   live lookup only reads the table, so any wake fails the bench.
//!
//! Run with `cargo bench -p moq-net --bench demand`.

use std::{
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Poll, Wake},
};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::broadcast;

/// `(tracks, handles)` shapes: the broadcast's tracks and the demand handles watching it.
const SHAPES: [(usize, usize); 5] = [(1, 1), (100, 1), (10_000, 1), (1, 100), (10_000, 100)];

/// `(tracks, handles)` shapes for churn: each `tracks` is one under the inbox capacity it grows to.
const CHURN_SHAPES: [(usize, usize); 5] = [(3, 1), (127, 1), (16_383, 1), (3, 100), (16_383, 100)];

/// `(tracks, readers)` shapes for lookup: the broadcast's tracks and the readers on the one looked up.
const LOOKUP_SHAPES: [(usize, usize); 5] = [(1, 1), (100, 1), (10_000, 1), (1, 100), (10_000, 100)];

/// Counts the wakes of a parked poll.
#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
	fn wake(self: Arc<Self>) {
		self.0.fetch_add(1, Ordering::Relaxed);
	}
}

fn changed(demand: &mut broadcast::Demand, waiter: &kio::Waiter) -> Option<bool> {
	match demand.poll_wanted_changed(waiter) {
		Poll::Ready(wanted) => Some(wanted.expect("broadcast closed")),
		Poll::Pending => None,
	}
}

fn bench_edge(c: &mut Criterion) {
	let mut group = c.benchmark_group("demand/edge");
	for (tracks, handles) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{tracks}t_{handles}h"));
		group.bench_function(id, |b| {
			let producer = broadcast::Info::new().produce();
			let _tracks: Vec<_> = (0..tracks)
				.map(|i| producer.create_track(format!("t{i}"), None).unwrap())
				.collect();
			let consumer = producer.consume();
			let waiter = kio::Waiter::noop();
			let mut demands: Vec<_> = (0..handles).map(|_| producer.demand()).collect();
			for demand in &mut demands {
				assert_eq!(changed(demand, &waiter), Some(false));
			}
			b.iter(|| {
				let reader = consumer.track("t0").unwrap().subscribe(None);
				for demand in &mut demands {
					assert_eq!(changed(demand, &waiter), Some(true));
				}
				drop(reader);
				for demand in &mut demands {
					assert_eq!(changed(demand, &waiter), Some(false));
				}
			});
		});
	}
	group.finish();
}

fn bench_churn(c: &mut Criterion) {
	let mut group = c.benchmark_group("demand/churn");
	for (tracks, handles) in CHURN_SHAPES {
		let id = BenchmarkId::from_parameter(format!("{tracks}t_{handles}h"));
		group.bench_function(id, |b| {
			let producer = broadcast::Info::new().produce();
			let _demands: Vec<_> = (0..handles).map(|_| producer.demand()).collect();
			let _tracks: Vec<_> = (0..tracks)
				.map(|i| producer.create_track(format!("t{i}"), None).unwrap())
				.collect();
			b.iter(|| drop(producer.create_track("churn", None).unwrap()));
		});
	}
	group.finish();
}

fn bench_lookup(c: &mut Criterion) {
	let mut group = c.benchmark_group("demand/lookup");
	for (tracks, readers) in LOOKUP_SHAPES {
		let id = BenchmarkId::from_parameter(format!("{tracks}t_{readers}r"));
		let producer = broadcast::Info::new().produce();
		let _tracks: Vec<_> = (0..tracks)
			.map(|i| producer.create_track(format!("t{i}"), None).unwrap())
			.collect();
		let consumer = producer.consume();
		let _readers: Vec<_> = (0..readers)
			.map(|_| consumer.track("t0").unwrap().subscribe(None))
			.collect();
		let wakes = Arc::new(Wakes::default());
		let waiter = kio::Waiter::new(wakes.clone().into());
		let mut demand = producer.demand();
		assert_eq!(changed(&mut demand, &waiter), Some(true));
		assert_eq!(changed(&mut demand, &waiter), None);
		let mut lookups = 0;
		group.bench_function(id, |b| {
			b.iter(|| {
				lookups += 1;
				let subscriber = consumer.track("t0").unwrap().subscribe(None);
				// Re-arm like a driver would, so every wake a lookup causes counts.
				assert_eq!(changed(&mut demand, &waiter), None);
				subscriber
			})
		});
		let wakes = wakes.0.load(Ordering::Relaxed);
		assert_eq!(
			wakes, 0,
			"{tracks}t_{readers}r: {lookups} live lookups woke a demand watcher {wakes} times"
		);
	}
	group.finish();
}

criterion_group!(benches, bench_edge, bench_churn, bench_lookup);
criterion_main!(benches);
