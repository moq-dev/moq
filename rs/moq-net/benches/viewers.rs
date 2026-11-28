//! Viewer sessions on a relay's origin: each requests one broadcast through a hop
//! of its own, subscribes to its track, and reads the latest group. Viewers share a
//! broadcast's front, so neither what a viewer costs nor what one leaves behind
//! may depend on how many came before.
//!
//! Each viewer excludes its own hop the way a session serves its peer. That view
//! is crate-private, so this reaches it through the `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench viewers`.

use std::fmt;

use futures::FutureExt;
use moq_net::Timestamp;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default()
		.with_max_age(None)
		.with_timescale(moq_net::Timescale::MILLI)
}

/// Viewers spread round-robin over the broadcasts, named `<viewers>v_<broadcasts>b`.
#[derive(Clone, Copy)]
struct Shape {
	viewers: usize,
	broadcasts: usize,
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}v_{}b", self.viewers, self.broadcasts)
	}
}

fn shapes() -> Vec<Shape> {
	[(100, 1), (1_000, 1), (1_000, 100), (10_000, 100)]
		.into_iter()
		.map(|(viewers, broadcasts)| Shape { viewers, broadcasts })
		.collect()
}

/// An origin publishing `room/<i>` broadcasts, each with one track holding one
/// group, and its driver polled in place.
struct Room {
	producer: moq_net::origin::Producer,
	driver: moq_net::origin::Driver,
	broadcasts: usize,
	_sources: Vec<(moq_net::origin::Route, moq_net::track::Producer)>,
}

impl Room {
	fn new(broadcasts: usize) -> Self {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		let _sources = (0..broadcasts)
			.map(|i| {
				let broadcast = producer
					.route(format!("room/{i}"), moq_net::origin::RouteOptions::default())
					.unwrap();
				let track = broadcast.broadcast().create_track("video", timed()).unwrap();
				let mut group = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
				group.write_frame(Timestamp::ZERO, b"frame".as_ref()).unwrap();
				group.finish().unwrap();
				(broadcast, track)
			})
			.collect();
		Self {
			producer,
			driver,
			broadcasts,
			_sources,
		}
	}

	fn poll(&mut self) {
		self.driver
			.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
			.unwrap();
	}

	/// Run `pending` to completion, polling the driver until it is.
	fn drive<T>(&mut self, pending: impl Future<Output = T>) -> T {
		let mut pending = std::pin::pin!(pending);
		loop {
			if let Some(output) = pending.as_mut().now_or_never() {
				return output;
			}
			self.poll();
		}
	}

	/// Viewer `n` joins and reads the latest group of its broadcast.
	fn join(&mut self, n: usize) -> moq_net::track::Subscriber {
		// Each viewer's own peer id, clear of the origin's random one below 2^53.
		let session = moq_net::fuzz::excluding(self.producer.consume(), (1 << 60) + n as u64);
		let path = format!("room/{}", n % self.broadcasts);
		let resolved = self.drive(session.request(path, None)).unwrap();
		let mut subscription = self.drive(resolved.track("video").unwrap().subscribe(None)).unwrap();
		self.drive(subscription.recv_group())
			.unwrap()
			.expect("the latest group");
		subscription
	}

	/// A viewer leaves, and the front sees it go.
	fn leave(&mut self, subscription: moq_net::track::Subscriber) {
		drop(subscription);
		self.poll();
	}
}

mod origin {
	use moq_net::track;
	use swole::bench;

	use super::{Room, Shape, shapes};

	/// A room with `viewers` watching, and the next viewer's number.
	struct Watching {
		room: Room,
		_watching: Vec<track::Subscriber>,
		next: usize,
	}

	fn watching(shape: Shape) -> Watching {
		let mut room = Room::new(shape.broadcasts);
		let _watching = (0..shape.viewers).map(|n| room.join(n)).collect();
		Watching {
			room,
			_watching,
			next: 0,
		}
	}

	/// One more viewer joining and leaving while `viewers` watch.
	#[bench(args = shapes(), setup = watching)]
	fn viewer_join(input: &mut Watching) {
		let subscription = input.room.join(input.next);
		input.next += 1;
		input.room.leave(subscription);
	}

	/// A room `viewers` came and went from.
	fn departed(shape: Shape) -> Room {
		let mut room = Room::new(shape.broadcasts);
		for n in 0..shape.viewers {
			let subscription = room.join(n);
			room.leave(subscription);
		}
		room
	}

	/// A route change covering every broadcast after `viewers` came and went: every front
	/// beneath it wakes to re-select, so the cost follows the fronts the viewers left behind.
	#[bench(args = shapes(), setup = departed)]
	fn viewer_churn(room: &mut Room) {
		let covering = room
			.producer
			.claim("room", moq_net::origin::RouteOptions::default())
			.unwrap();
		room.poll();
		drop(covering);
		room.poll();
	}
}

fn main() {
	swole::main();
}
