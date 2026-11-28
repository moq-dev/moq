//! What a broadcast's track listing costs as it fans out.
//!
//! - change: a track is created and dropped on one broadcast, and every listing of it
//!   reads both events. Swept over the listings and the tracks it already lists, and
//!   over idle broadcasts with listings of their own, so a cost that grows with the
//!   table rather than the touched broadcast shows up as a slope.
//! - list: a new listing reads a broadcast's tracks through SYNCED, swept the same way.
//!
//! Run with `cargo bench -p moq-net --bench tracks`.

use std::{fmt, task::Poll};

use moq_net::broadcast::{self, Tracks, TracksEvent};

/// Listings and tracks per broadcast, and the broadcasts beside the touched one, named
/// `<listings>l_<tracks>t_<broadcasts>b`.
#[derive(Clone, Copy)]
struct Shape {
	listings: usize,
	tracks: usize,
	broadcasts: usize,
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}l_{}t_{}b", self.listings, self.tracks, self.broadcasts)
	}
}

fn shapes() -> Vec<Shape> {
	[
		(1, 10, 1),
		(1, 1_000, 1),
		(100, 10, 1),
		(100, 1_000, 1),
		(10, 10, 1_000),
		(10, 10, 1),
	]
	.into_iter()
	.map(|(listings, tracks, broadcasts)| Shape {
		listings,
		tracks,
		broadcasts,
	})
	.collect()
}

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

fn table(
	Shape {
		listings,
		tracks,
		broadcasts,
	}: Shape,
) -> Table {
	let mut table = Table {
		broadcasts: Vec::new(),
		_tracks: Vec::new(),
		listings: Vec::new(),
	};
	for _ in 0..broadcasts {
		let broadcast = broadcast::Info::new().produce();
		for i in 0..tracks {
			table._tracks.push(
				broadcast
					.create_track(format!("t{i}"), moq_net::track::Info::default().with_max_age(None))
					.unwrap(),
			);
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

mod tracks {
	use moq_net::broadcast;
	use swole::bench;

	use super::{Shape, Table, drain, shapes, table};

	#[bench(args = shapes(), setup = table)]
	fn change(table: &mut Table) {
		let broadcast = &table.broadcasts[0];
		let readers = &mut table.listings[0];
		let track = broadcast
			.create_track("new", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		drop(track);
		for reader in readers.iter_mut() {
			// The unread add and removal cancel; read the next pair as it lands.
			assert_eq!(drain(reader), 0);
		}
		let track = broadcast
			.create_track("new", moq_net::track::Info::default().with_max_age(None))
			.unwrap();
		for reader in readers.iter_mut() {
			assert_eq!(drain(reader), 1);
		}
		drop(track);
		for reader in readers.iter_mut() {
			assert_eq!(drain(reader), 1);
		}
	}

	/// A table and a consumer of its first broadcast, which each call lists.
	struct Listing {
		_table: Table,
		consumer: broadcast::Consumer,
		tracks: usize,
	}

	fn listing(shape: Shape) -> Listing {
		let table = table(shape);
		let consumer = table.broadcasts[0].consume();
		Listing {
			_table: table,
			consumer,
			tracks: shape.tracks,
		}
	}

	#[bench(args = shapes(), setup = listing)]
	fn list(input: &Listing) {
		let mut reader = input.consumer.tracks().unwrap();
		assert_eq!(drain(&mut reader), input.tracks + 1);
	}
}

fn main() {
	swole::main();
}
