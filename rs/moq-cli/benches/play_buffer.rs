//! Encoded retention cost of a fresh playback buffer, swept over delay and bitrate.
//!
//! Run with `cargo bench -p moq-cli --bench play_buffer`.

#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
#[path = "../src/play/buffer.rs"]
mod buffer;

use std::fmt;
use std::time::Duration;

use moq_mux::container::Frame;
use swole::bench;

/// Frames pushed per call: ten seconds of 30 fps video with a keyframe each second.
const FRAMES: usize = 300;

/// A retention window and frame size, named `delay_<ms>ms/payload_<bytes>B`.
#[derive(Clone, Copy)]
struct Case {
	delay_ms: u64,
	payload_bytes: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "delay_{}ms/payload_{}B", self.delay_ms, self.payload_bytes)
	}
}

fn cases() -> Vec<Case> {
	[100, 2_000, 10_000]
		.into_iter()
		.flat_map(|delay_ms| {
			[1_024, 65_536].map(|payload_bytes| Case {
				delay_ms,
				payload_bytes,
			})
		})
		.collect()
}

fn frames(case: Case) -> (Vec<Frame>, Duration) {
	let frames = (0..FRAMES as u64)
		.map(|index| Frame {
			timestamp: moq_net::Timestamp::from_millis(index * 33).unwrap(),
			decode: None,
			duration: None,
			payload: bytes::Bytes::from(vec![128; case.payload_bytes]),
			keyframe: index % 30 == 0,
		})
		.collect();
	(frames, Duration::from_millis(case.delay_ms))
}

#[bench(args = cases(), setup = frames, items = |_| FRAMES)]
fn push(input: &(Vec<Frame>, Duration)) -> buffer::Buffer {
	let (frames, delay) = input;
	let mut buffer = buffer::Buffer::default();
	for frame in frames {
		buffer.push(frame.clone(), *delay);
	}
	buffer
}

fn main() {
	swole::main();
}
