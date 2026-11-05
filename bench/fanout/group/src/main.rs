//! A relay's frame fan-out: every subscriber drains one finished 512-frame group in batches,
//! as moq-net's `group_relay_frames/plain/512/*` bench does.
//! Usage: `fanout-group SUBSCRIBERS CYCLES`. The work is deterministic, so its retired
//! instructions, less a run of zero cycles, are the per-group cost whatever the host does.
use std::hint::black_box;

use futures::FutureExt;
use moq_net::frame;

const FRAMES: usize = 512;

fn main() {
	let mut args = std::env::args().skip(1).map(|a| a.parse::<usize>().unwrap());
	let (subscribers, cycles) = (args.next().unwrap(), args.next().unwrap());
	let (broadcast, track, group) = moq_net::fuzz::object_group(FRAMES, false);
	let mut buffer = frame::Buffer::<16>::new();
	for _ in 0..cycles {
		for _ in 0..subscribers {
			let mut reader = group.consume();
			loop {
				let batch = reader.read_frames(&mut buffer).now_or_never().unwrap().unwrap();
				if batch.is_empty() {
					break;
				}
				for frame in batch.iter() {
					black_box(frame);
				}
			}
		}
	}
	// Left standing, so teardown stays out of the count.
	std::mem::forget((broadcast, track, group));
	std::process::exit(0);
}
