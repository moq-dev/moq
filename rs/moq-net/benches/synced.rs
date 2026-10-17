//! What it costs an origin to track whether its announce consumers have synced.
//!
//! Every session requesting announcements holds a snapshot on the origin until its
//! peer's initial set is in, and the announce consumers its prefix overlaps wait on
//! it. Both shapes are swept over the snapshots in flight (announce streams) and the
//! consumers watching elsewhere, so a cost that grows with the table rather than the
//! touched path shows up as a slope.
//!
//! - land: a snapshot starts and lands under one prefix, releasing its one consumer.
//! - register: a consumer of one prefix registers, counting the snapshots it waits on.
//!
//! The snapshot is crate-private, so this reaches it through the hidden `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench synced`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{Hop, Pattern, Patterns, announce, fuzz, origin};

/// `(streams, consumers)` shapes.
const SHAPES: [(usize, usize); 4] = [(10, 10), (1_000, 10), (10, 1_000), (1_000, 1_000)];

/// An origin with `streams` snapshots in flight under `room/` and `consumers` cursors
/// on prefixes of their own under `other/`. The handles are held for the whole run.
struct Table {
	producer: origin::Producer,
	_driver: origin::Driver,
	_streams: Vec<fuzz::Snapshot>,
	_consumers: Vec<announce::Consumer>,
}

fn table(streams: usize, consumers: usize) -> Table {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	let streams = (0..streams)
		.map(|i| fuzz::snapshot(&producer, &format!("room/{i}")))
		.collect();
	let consumers = (0..consumers)
		.map(|i| scoped(&producer, &format!("other/{i}")).announced())
		.collect();
	Table {
		producer,
		_driver: driver,
		_streams: streams,
		_consumers: consumers,
	}
}

fn scoped(producer: &origin::Producer, prefix: &str) -> origin::Consumer {
	producer
		.consume()
		.scope(prefix, &Patterns::from(Pattern::all()))
		.unwrap()
}

fn bench_land(c: &mut Criterion) {
	let mut group = c.benchmark_group("synced/land");
	for (streams, consumers) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{streams}st_{consumers}c"));
		group.bench_function(id, |b| {
			let table = table(streams, consumers);
			let watcher = scoped(&table.producer, "room/incoming").announced();
			b.iter(|| {
				let snapshot = fuzz::snapshot(&table.producer, "room/incoming");
				assert!(!watcher.is_synced());
				drop(snapshot);
				assert!(watcher.is_synced());
			});
		});
	}
	group.finish();
}

fn bench_register(c: &mut Criterion) {
	let mut group = c.benchmark_group("synced/register");
	for (streams, consumers) in SHAPES {
		let id = BenchmarkId::from_parameter(format!("{streams}st_{consumers}c"));
		group.bench_function(id, |b| {
			let table = table(streams, consumers);
			let incoming = scoped(&table.producer, "room/incoming");
			let _held = fuzz::snapshot(&table.producer, "room/incoming");
			b.iter(|| {
				let watcher = incoming.announced();
				assert!(!watcher.is_synced());
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_land, bench_register);
criterion_main!(benches);
