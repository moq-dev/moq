//! The decode clock swept over how many frames it holds and how their PTS run: draining what
//! it holds, and settling frames as they are read until the overdue rule bounds them.
//!
//! Run with `cargo bench -p moq-mux --bench decode_clock`.
use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use kio::clock::Instant;
use swole::bench;

#[path = "../src/container/decode.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod decode;

/// A 40 ms frame in 90 kHz ticks.
const FRAME: u64 = 3_600;

/// The `index`-th frame's PTS in decode order: stuck, running backward, or a valid B-pyramid.
fn pts(shape: &str, index: u64) -> u64 {
	let base = 1_000 * 90_000;
	match shape {
		"flat" => base,
		"backward" => base - index,
		_ => {
			const PYRAMID: [u64; 9] = [0, 4, 2, 1, 3, 8, 6, 5, 7];
			base + (index / 9 * 9 + PYRAMID[(index % 9) as usize]) * FRAME
		}
	}
}

/// The PTS shapes, as [`pts`] reads them.
const SHAPES: [&str; 3] = ["flat", "backward", "pyramid"];

/// A count of frames in one PTS shape, named `<shape>/<frames>`.
#[derive(Clone, Copy)]
struct Run {
	shape: &'static str,
	frames: u64,
}

impl fmt::Display for Run {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.shape, self.frames)
	}
}

/// A held count of `size`-byte frames in one PTS shape, named `<shape>/<size>B/<held>`.
#[derive(Clone, Copy)]
struct Held {
	shape: &'static str,
	held: u64,
	size: usize,
}

impl fmt::Display for Held {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}B/{}", self.shape, self.size, self.held)
	}
}

fn held_cases() -> Vec<Held> {
	let mut cases = Vec::new();
	for shape in SHAPES {
		for held in [1_000u64, 10_000, 100_000] {
			for size in [64usize, 4_096] {
				cases.push(Held { shape, held, size });
			}
		}
	}
	cases
}

fn payload(case: Held) -> (Held, Bytes) {
	(case, Bytes::from(vec![0; case.size]))
}

/// Flush `held` frames read with nothing settled, as at a track's end.
#[bench(args = held_cases(), setup = payload, items = |case: Held| case.held)]
fn decode_clock_drain((case, payload): &(Held, Bytes)) {
	let mut clock = decode::DecodeClock::default();
	for index in 0..case.held {
		clock.push(payload.clone(), pts(case.shape, index), Instant::ZERO, 0);
	}
	let mut drained = 0;
	while clock.pop(decode::MAX_REORDER, 0, decode::Settle::All).is_some() {
		drained += 1;
	}
	assert_eq!(drained, case.held);
}

fn runs() -> Vec<Run> {
	SHAPES
		.into_iter()
		.flat_map(|shape| [1_000u64, 10_000, 100_000].map(|frames| Run { shape, frames }))
		.collect()
}

/// Read `frames` frames 1 ms apart, a 120 ms lookahead and 500 ms delay, settling each as an
/// exporter does: flat or backward PTS holds what the overdue rule lets through.
#[bench(args = runs(), items = |run: Run| run.frames)]
fn decode_clock_read(run: Run) {
	let lookahead = 120 * 90;
	let delay = Duration::from_millis(500);
	let mut clock = decode::DecodeClock::default();
	let (mut held, mut most) = (0, 0);
	for index in 0..run.frames {
		let latest = Instant::ZERO.saturating_add(Duration::from_millis(index));
		clock.push(index, pts(run.shape, index), latest, 0);
		held += 1;
		loop {
			let settle = clock.settle(lookahead, latest, delay, usize::MAX);
			if clock.pop(lookahead, 0, settle).is_none() {
				break;
			}
			held -= 1;
		}
		most = most.max(held);
	}
	// What arrives in the deepest lookahead and the delay, a frame a millisecond.
	assert!(most <= 2_501, "{}: {most} held", run.shape);
}

fn main() {
	swole::main();
}
