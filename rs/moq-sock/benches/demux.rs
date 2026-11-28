//! Routing on a shared port: what every QUIC datagram pays to be told it is QUIC.
//!
//! Swept over the pins held (WebRTC sessions) and the shards routing at once,
//! since every shard of a worker group reads the same pin table.
//!
//! Run with `cargo bench -p moq-sock --bench demux`.

#![allow(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "a benchmark times itself on the host clock"
)]

use std::{
	fmt,
	hint::black_box,
	net::SocketAddr,
	time::{Duration, Instant},
};

use moq_sock::demux::{Flows, Pin, Route};
use swole::{Iterations, bench};

/// Datagrams one shard routes per iteration, as one receive batch would hold.
const BATCH: usize = 64;

fn remote(i: usize) -> SocketAddr {
	SocketAddr::from(([10, (i >> 16) as u8, (i >> 8) as u8, i as u8], 4433))
}

/// Pins held and shards routing at once, named `pins_<n>/shards_<n>`.
#[derive(Clone, Copy)]
struct Case {
	pins: usize,
	shards: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "pins_{}/shards_{}", self.pins, self.shards)
	}
}

fn cases() -> Vec<Case> {
	[0, 1, 1024]
		.into_iter()
		.flat_map(|pins| [1, 4].map(|shards| Case { pins, shards }))
		.collect()
}

/// A pin table holding `pins` pins, and the shards that route through it.
struct Table {
	flows: Flows,
	_pins: Vec<Pin>,
	shards: usize,
}

fn table(case: Case) -> Table {
	let flows = Flows::default();
	// Pinned addresses never match the senders, so every datagram stays QUIC.
	let _pins = (0..case.pins).map(|i| flows.pin(remote(1 << 20 | i))).collect();
	Table {
		flows,
		_pins,
		shards: case.shards,
	}
}

/// Times itself so each shard's thread starts once per sample and routes a batch per iteration.
#[bench(args = cases(), setup = table, items = |case: Case| BATCH * case.shards)]
fn demux_route(table: &Table, iterations: Iterations) -> Duration {
	let quic = [0x40u8; 1200];
	let start = Instant::now();
	std::thread::scope(|scope| {
		for shard in 0..table.shards {
			let flows = &table.flows;
			let quic = &quic;
			scope.spawn(move || {
				for _ in iterations {
					for i in 0..BATCH {
						let route = flows.route(remote(shard * BATCH + i), quic);
						assert_eq!(black_box(route), Route::Quic);
					}
				}
			});
		}
	});
	start.elapsed()
}

fn main() {
	swole::main();
}
