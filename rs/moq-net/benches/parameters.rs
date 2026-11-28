//! SETUP parameter bags, swept from a small handshake to a message-sized bag.
//!
//! Keep the full fuzz round trip in its own executable: its broad codec call graph
//! perturbs message-codec timings even when this target's benchmarks are not selected.
//! Run with `cargo bench -p moq-net --features fuzz --bench parameters`.

use moq_net::fuzz::{encode_varints, lite_wire};
use swole::bench;

/// A lite-07 SETUP message carrying `count` parameters.
fn wire(count: u64) -> Vec<u8> {
	let values: Vec<u64> = std::iter::once(count)
		.chain((100..100 + count).flat_map(|kind| [kind, 0]))
		.collect();
	// The existing fuzz adapter round-trips the private parameter codec on lite-07.
	let mut wire = vec![6, 20];
	encode_varints(&values, true, &mut wire);
	assert!(lite_wire(&wire));
	wire
}

#[bench(args = [4u64, 16, 64, 256, 1024, 4096], setup = wire, items = |count| count)]
fn setup_parameters(wire: &[u8]) -> bool {
	lite_wire(wire)
}

fn main() {
	swole::main();
}
