//! Encoded retention cost and peak memory, swept over delay and bitrate.
//! Run with `cargo bench -p moq-cli --bench play_buffer`.

#[allow(dead_code)]
#[path = "../src/play/buffer.rs"]
mod buffer;

use std::hint::black_box;
use std::time::{Duration, Instant};

fn main() {
	for delay_ms in [100, 2_000, 10_000] {
		for payload_bytes in [1_024, 65_536] {
			let frames: Vec<_> = (0..300)
				.map(|index| moq_mux::container::Frame {
					timestamp: hang::moq_net::Timestamp::from_millis(index * 33).unwrap(),
					duration: None,
					payload: bytes::Bytes::from(vec![128; payload_bytes]),
					keyframe: index % 30 == 0,
				})
				.collect();
			let start = Instant::now();
			let mut peak = 0;
			for _ in 0..1_000 {
				let mut buffer = buffer::Buffer::default();
				for frame in &frames {
					buffer.push(black_box(frame.clone()), Duration::from_millis(delay_ms));
					peak = peak.max(buffer.bytes);
				}
				black_box(buffer);
			}
			println!(
				"delay={delay_ms}ms payload={payload_bytes}B: {:.1} ns/frame, peak={peak}B",
				start.elapsed().as_nanos() as f64 / 300_000.0
			);
		}
	}
}
