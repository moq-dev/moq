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
//! The snapshot is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench synced`.

use std::fmt;

use moq_net::{Prefixes, fuzz, origin};

/// Snapshots in flight and consumers watching elsewhere, named `<streams>st_<consumers>c`.
#[derive(Clone, Copy)]
struct Shape {
	streams: usize,
	consumers: usize,
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}st_{}c", self.streams, self.consumers)
	}
}

const SHAPES: [Shape; 4] = [
	Shape {
		streams: 10,
		consumers: 10,
	},
	Shape {
		streams: 1_000,
		consumers: 10,
	},
	Shape {
		streams: 10,
		consumers: 1_000,
	},
	Shape {
		streams: 1_000,
		consumers: 1_000,
	},
];

/// An origin with `streams` snapshots in flight under `room/` and `consumers` cursors
/// on prefixes of their own under `other/`. The handles are held for the whole run.
struct Table {
	producer: origin::Producer,
	_driver: origin::Driver,
	_streams: Vec<fuzz::Snapshot>,
	_consumers: Vec<origin::Routes>,
}

fn table(Shape { streams, consumers }: Shape) -> Table {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
	let streams = (0..streams)
		.map(|i| fuzz::snapshot(&producer, &format!("room/{i}")))
		.collect();
	let consumers = (0..consumers)
		.map(|i| scoped(&producer, &format!("other/{i}")).routes())
		.collect();
	Table {
		producer,
		_driver: driver,
		_streams: streams,
		_consumers: consumers,
	}
}

fn scoped(producer: &origin::Producer, prefix: &str) -> origin::Consumer {
	producer.consume().scope(prefix, &Prefixes::all()).unwrap()
}

mod synced {
	use moq_net::{fuzz, origin};
	use swole::bench;

	use super::{SHAPES, Table, scoped, table};

	/// A table and a consumer of the prefix a snapshot lands under.
	struct Landing {
		table: Table,
		watcher: origin::Routes,
	}

	fn landing(shape: super::Shape) -> Landing {
		let table = table(shape);
		let watcher = scoped(&table.producer, "room/incoming").routes();
		Landing { table, watcher }
	}

	#[bench(args = SHAPES, setup = landing)]
	fn land(input: &Landing) {
		let snapshot = fuzz::snapshot(&input.table.producer, "room/incoming");
		assert!(!input.watcher.poll_synced(&kio::Waiter::noop()).is_ready());
		drop(snapshot);
		assert!(input.watcher.poll_synced(&kio::Waiter::noop()).is_ready());
	}

	/// A table with a snapshot held under the prefix a consumer registers on.
	struct Registering {
		_table: Table,
		incoming: origin::Consumer,
		_held: fuzz::Snapshot,
	}

	fn registering(shape: super::Shape) -> Registering {
		let table = table(shape);
		let incoming = scoped(&table.producer, "room/incoming");
		let _held = fuzz::snapshot(&table.producer, "room/incoming");
		Registering {
			_table: table,
			incoming,
			_held,
		}
	}

	#[bench(args = SHAPES, setup = registering)]
	fn register(input: &Registering) {
		let watcher = input.incoming.routes();
		assert!(!watcher.poll_synced(&kio::Waiter::noop()).is_ready());
	}
}

fn main() {
	swole::main();
}
