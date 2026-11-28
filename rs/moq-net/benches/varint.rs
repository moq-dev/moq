//! Lite-07 leading-ones varints against lite-06 QUIC varints: the CPU to encode and decode
//! what a publisher writes per frame, per group, and per request.
//!
//! The two versions share these layouts, so the only difference is the varint codec.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench varint`.

use std::fmt;

use moq_net::{Version, fuzz::LiteSample};

/// One sample encoded with one version, named `<sample>/<version>`.
#[derive(Clone, Copy)]
struct Case {
	sample: LiteSample,
	version: &'static str,
}

impl Case {
	fn version(&self) -> Version {
		self.version.parse().unwrap()
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{:?}/{}", self.sample, self.version)
	}
}

fn cases() -> Vec<Case> {
	LiteSample::ALL
		.into_iter()
		.flat_map(|sample| ["moq-lite-06", "moq-lite-07-wip"].map(|version| Case { sample, version }))
		.collect()
}

mod lite_varint {
	use moq_net::{Version, fuzz::LiteSample};
	use swole::bench;

	use super::{Case, cases};

	fn version(case: Case) -> (LiteSample, Version) {
		(case.sample, case.version())
	}

	#[bench(args = cases(), setup = version, bytes = |case: Case| case.sample.encode(case.version()).len())]
	fn encode((sample, version): (LiteSample, Version)) -> Vec<u8> {
		sample.encode(version)
	}

	fn wire(case: Case) -> (LiteSample, Version, Vec<u8>) {
		(case.sample, case.version(), case.sample.encode(case.version()))
	}

	#[bench(args = cases(), setup = wire, bytes = |case: Case| case.sample.encode(case.version()).len())]
	fn decode(input: &(LiteSample, Version, Vec<u8>)) -> usize {
		let (sample, version, wire) = input;
		sample.decode(*version, wire)
	}
}

fn main() {
	swole::main();
}
