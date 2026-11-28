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
	fmt,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Poll, Wake},
};

use moq_net::broadcast;

/// The broadcast's tracks and the demand handles watching it, named `<tracks>t_<handles>h`.
#[derive(Clone, Copy)]
struct Shape {
	tracks: usize,
	handles: usize,
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}t_{}h", self.tracks, self.handles)
	}
}

fn shapes(shapes: [(usize, usize); 5]) -> [Shape; 5] {
	shapes.map(|(tracks, handles)| Shape { tracks, handles })
}

/// The broadcast's tracks and the readers on the one looked up, named `<tracks>t_<readers>r`.
#[derive(Clone, Copy)]
struct Lookup {
	tracks: usize,
	readers: usize,
}

impl fmt::Display for Lookup {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}t_{}r", self.tracks, self.readers)
	}
}

const LOOKUPS: [(usize, usize); 5] = [(1, 1), (100, 1), (10_000, 1), (1, 100), (10_000, 100)];

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

fn tracks(producer: &broadcast::Producer, tracks: usize) -> Vec<moq_net::track::Producer> {
	(0..tracks)
		.map(|i| {
			producer
				.create_track(format!("t{i}"), moq_net::track::Info::default().with_max_age(None))
				.unwrap()
		})
		.collect()
}

mod demand {
	use std::sync::{Arc, atomic::Ordering};

	use moq_net::{broadcast, track};
	use swole::bench;

	use super::{LOOKUPS, Lookup, Shape, Wakes, changed, shapes, tracks};

	struct Edge {
		_producer: broadcast::Producer,
		_tracks: Vec<track::Producer>,
		consumer: broadcast::Consumer,
		demands: Vec<broadcast::Demand>,
	}

	fn edge_setup(shape: Shape) -> Edge {
		let producer = broadcast::Info::new().produce();
		let _tracks = tracks(&producer, shape.tracks);
		let consumer = producer.consume();
		let waiter = kio::Waiter::noop();
		let mut demands: Vec<_> = (0..shape.handles).map(|_| producer.demand()).collect();
		for demand in &mut demands {
			assert_eq!(changed(demand, &waiter), Some(false));
		}
		Edge {
			_producer: producer,
			_tracks,
			consumer,
			demands,
		}
	}

	#[bench(args = shapes([(1, 1), (100, 1), (10_000, 1), (1, 100), (10_000, 100)]), setup = edge_setup)]
	fn edge(input: &mut Edge) {
		let waiter = kio::Waiter::noop();
		let reader = input.consumer.track("t0").unwrap().subscribe(None);
		for demand in &mut input.demands {
			assert_eq!(changed(demand, &waiter), Some(true));
		}
		drop(reader);
		for demand in &mut input.demands {
			assert_eq!(changed(demand, &waiter), Some(false));
		}
	}

	struct Churn {
		producer: broadcast::Producer,
		_demands: Vec<broadcast::Demand>,
		_tracks: Vec<track::Producer>,
	}

	fn churn_setup(shape: Shape) -> Churn {
		let producer = broadcast::Info::new().produce();
		let _demands = (0..shape.handles).map(|_| producer.demand()).collect();
		let _tracks = tracks(&producer, shape.tracks);
		Churn {
			producer,
			_demands,
			_tracks,
		}
	}

	/// Each `tracks` is one under the inbox capacity it grows to.
	#[bench(args = shapes([(3, 1), (127, 1), (16_383, 1), (3, 100), (16_383, 100)]), setup = churn_setup)]
	fn churn(input: &Churn) {
		drop(
			input
				.producer
				.create_track("churn", track::Info::default().with_max_age(None))
				.unwrap(),
		)
	}

	struct Looking {
		_producer: broadcast::Producer,
		_tracks: Vec<track::Producer>,
		consumer: broadcast::Consumer,
		_readers: Vec<kio::Pending<track::Subscribing>>,
		wakes: Arc<Wakes>,
		waiter: kio::Waiter,
		demand: broadcast::Demand,
	}

	fn lookup_setup(lookup: Lookup) -> Looking {
		let producer = broadcast::Info::new().produce();
		let _tracks = tracks(&producer, lookup.tracks);
		let consumer = producer.consume();
		let _readers = (0..lookup.readers)
			.map(|_| consumer.track("t0").unwrap().subscribe(None))
			.collect();
		let wakes = Arc::new(Wakes::default());
		let waiter = kio::Waiter::new(wakes.clone().into());
		let mut demand = producer.demand();
		assert_eq!(changed(&mut demand, &waiter), Some(true));
		assert_eq!(changed(&mut demand, &waiter), None);
		Looking {
			_producer: producer,
			_tracks,
			consumer,
			_readers,
			wakes,
			waiter,
			demand,
		}
	}

	/// A live lookup only reads the table, so any wake fails the bench.
	fn unwoken(input: &mut Looking) {
		assert_eq!(
			input.wakes.0.load(Ordering::Relaxed),
			0,
			"a live lookup woke a demand watcher"
		);
	}

	#[bench(args = LOOKUPS.map(|(tracks, readers)| Lookup { tracks, readers }), setup = lookup_setup, after = unwoken)]
	fn lookup(input: &mut Looking) -> kio::Pending<track::Subscribing> {
		let subscriber = input.consumer.track("t0").unwrap().subscribe(None);
		// Re-arm like a driver would, so every wake a lookup causes counts.
		assert_eq!(changed(&mut input.demand, &input.waiter), None);
		subscriber
	}
}

fn main() {
	swole::main();
}
