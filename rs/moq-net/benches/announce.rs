//! Lite-07 announce compression against lite-06 literal framing: the bytes an
//! announce stream costs, and the CPU to encode and decode it.
//!
//! Bytes and decode are measured over a whole stream: `routes` routes, then `churn` of
//! them replaced (an END and a fresh START), so the bases the encoder picks come and
//! go. Encode is measured in the steady state a relay sees: the encoder already holds
//! `routes` live routes, and each iteration retracts the oldest and starts a new one.
//! Two workloads bracket the gain:
//!
//! - health: `<pid>/private/channel_N/stream-health-<ts>` from a few origins behind
//!   a shared pair of relays, the shape that motivated compression.
//! - unique: random single-segment paths and random two-hop chains, where no path
//!   is shared and every START pays the two zero base/keep bytes.
//!
//! The hop chains travel only on lite-06; lite-07 names each claim's route instead, so
//! its streams carry none.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench announce`.

use std::fmt;

use moq_net::{
	Path, announce,
	fuzz::{AnnounceWriter, Announced},
	origin,
};

/// The previous replay, retained here as a scaling reference.
fn scanned(events: Vec<origin::RouteEvent>) -> Vec<announce::Announce> {
	let mut initial: Vec<announce::Announce> = Vec::new();
	for event in events {
		let origin::RouteEvent {
			kind, announce: update, ..
		} = event;
		// A kind this match predates counts as active: the route is still advertised.
		let active = !matches!(kind, origin::RouteKind::End | origin::RouteKind::Offline);
		initial.retain(|held| held.prefix != update.prefix);
		if active {
			initial.push(update);
		}
	}
	initial
}

/// Routes and updates per route in a replayed batch, named `<routes>r_<updates>u`.
#[derive(Clone, Copy)]
struct Replay {
	routes: usize,
	updates: u64,
}

impl fmt::Display for Replay {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}r_{}u", self.routes, self.updates)
	}
}

fn replays() -> Vec<Replay> {
	[1, 64, 1024]
		.into_iter()
		.flat_map(|routes| [1, 8, 32].map(|updates| Replay { routes, updates }))
		.collect()
}

mod announce_replay {
	use moq_net::{announce, origin};
	use swole::bench;

	use super::{Replay, replays, scanned};

	/// Each case's updates, `updates` per route, and how many routes they leave.
	fn events(replay: Replay) -> (Vec<origin::RouteEvent>, usize) {
		let events = (0..replay.updates)
			.flat_map(|cost| {
				(0..replay.routes).map(move |route| moq_net::fuzz::route_update(&format!("route/{route}"), cost))
			})
			.collect();
		(events, replay.routes)
	}

	fn count(replay: Replay) -> usize {
		replay.routes * replay.updates as usize
	}

	#[bench(args = replays(), setup = events, items = count)]
	fn indexed((events, routes): (Vec<origin::RouteEvent>, usize)) -> Vec<announce::Announce> {
		let initial = moq_net::fuzz::replay(events);
		assert_eq!(initial.len(), routes);
		initial
	}

	#[bench(args = replays(), setup = events, items = count)]
	fn scan((events, routes): (Vec<origin::RouteEvent>, usize)) -> Vec<announce::Announce> {
		let initial = scanned(events);
		assert_eq!(initial.len(), routes);
		initial
	}
}

/// Routes announced, then how many of them are replaced, named `<routes>x<churn>`.
#[derive(Clone, Copy)]
struct Shape {
	routes: u64,
	churn: u64,
}

impl Shape {
	/// Messages in the stream: a START per route and an END and START per replacement.
	fn messages(self) -> u64 {
		self.routes + 2 * self.churn
	}
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}", self.routes, self.churn)
	}
}

/// Shapes for the whole-stream measurements.
const SHAPES: [Shape; 4] = [
	Shape { routes: 100, churn: 0 },
	Shape {
		routes: 1_000,
		churn: 0,
	},
	Shape {
		routes: 1_000,
		churn: 1_000,
	},
	Shape {
		routes: 10_000,
		churn: 1_000,
	},
];

/// Live routes for the steady-state encode.
const LIVE: [u64; 3] = [100, 1_000, 10_000];

/// How many origins publish health streams, each behind the same two relays.
const ORIGINS: u64 = 8;

/// A deterministic 62-bit id, standing in for a random Hop ID.
fn id(seed: u64) -> u64 {
	// SplitMix64, masked to the varint range and kept non-zero.
	let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
	z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
	((z ^ (z >> 31)) & ((1 << 62) - 1)).max(1)
}

/// Start route `n`, with its hop chain only on lite-06: a lite-07 claim names its route
/// instead, and carries a chain only when it was learned over lite-06.
fn start(route: fn(u64) -> (Path, Vec<u64>), n: u64, compress: bool) -> Announced {
	let (path, hops) = route(n);
	Announced::Start(path, if compress { Vec::new() } else { hops })
}

/// Announce `routes` routes, then replace `churn` of them, oldest first.
fn stream(routes: u64, churn: u64, route: fn(u64) -> (Path, Vec<u64>), compress: bool) -> Vec<Announced> {
	let mut announced: Vec<_> = (0..routes).map(|n| start(route, n, compress)).collect();
	for n in 0..churn {
		announced.push(Announced::End(n));
		announced.push(start(route, routes + n, compress));
	}
	announced
}

fn health(n: u64) -> (Path, Vec<u64>) {
	let origin = n % ORIGINS;
	let path = format!(
		"{:x}/private/channel_{}/stream-health-{}",
		id(origin),
		n % 16,
		1_700_000_000 + n
	);
	(Path::new(&path).unwrap(), vec![id(origin), id(100), id(101)])
}

fn unique(n: u64) -> (Path, Vec<u64>) {
	(
		moq_net::Path::new(&format!("{:x}", id(n))).unwrap(),
		vec![id(n + 1_000_000), id(n + 2_000_000)],
	)
}

/// An encoder holding `routes` live routes, which each call churns by one.
struct Churn {
	writer: AnnounceWriter,
	data: Vec<u8>,
	ring: Vec<Announced>,
	routes: u64,
	oldest: u64,
}

impl Churn {
	fn new(routes: u64, route: fn(u64) -> (Path, Vec<u64>), compress: bool) -> Self {
		// A ring twice the live set, so the route started never collides with one still live.
		let ring: Vec<_> = (0..2 * routes).map(|n| start(route, n, compress)).collect();
		let mut writer = AnnounceWriter::new(compress);
		let mut data = Vec::new();
		for start in &ring[..routes as usize] {
			writer.write(start, &mut data);
		}
		Self {
			writer,
			data,
			ring,
			routes,
			oldest: 0,
		}
	}

	/// Retract the oldest live route and start the next one.
	fn step(&mut self) {
		self.data.clear();
		self.writer.write(&Announced::End(self.oldest), &mut self.data);
		self.writer.write(
			&self.ring[((self.routes + self.oldest) % (2 * self.routes)) as usize],
			&mut self.data,
		);
		self.oldest += 1;
	}
}

/// One workload's announce benchmarks, each version's churn and decode.
macro_rules! workload {
	($name:ident, $route:path) => {
		mod $name {
			use moq_net::fuzz::{Announced, decode_announces, encode_announces};
			use swole::bench;

			use super::{Churn, LIVE, SHAPES, Shape, stream};

			fn churn06(routes: u64) -> Churn {
				Churn::new(routes, $route, false)
			}

			fn churn07(routes: u64) -> Churn {
				Churn::new(routes, $route, true)
			}

			#[bench(args = LIVE, setup = churn06, items = |_: u64| 2)]
			fn churn_lite06(churn: &mut Churn) {
				churn.step()
			}

			#[bench(args = LIVE, setup = churn07, items = |_: u64| 2)]
			fn churn_lite07(churn: &mut Churn) {
				churn.step()
			}

			fn encoded06(shape: Shape) -> Vec<u8> {
				encode_announces(&stream(shape.routes, shape.churn, $route, false), false)
			}

			fn encoded07(shape: Shape) -> Vec<u8> {
				encode_announces(&stream(shape.routes, shape.churn, $route, true), true)
			}

			#[bench(args = SHAPES, setup = encoded06, items = Shape::messages)]
			fn decode_lite06(encoded: &[u8]) -> Vec<Announced> {
				decode_announces(encoded, false)
			}

			#[bench(args = SHAPES, setup = encoded07, items = Shape::messages)]
			fn decode_lite07(encoded: &[u8]) -> Vec<Announced> {
				decode_announces(encoded, true)
			}
		}
	};
}

workload!(announce_health, super::health);
workload!(announce_unique, super::unique);

fn main() {
	swole::main();
}
