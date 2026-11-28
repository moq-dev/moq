//! A keyframe carrying many H.264 PPS, swept over the PPS count, with distinct or repeated ids.
//!
//! The splitter keeps one parameter set per id, so a per-set scan of the ones before it would
//! show up as a quadratic slope over the count. Every PPS has distinct bytes, so a byte-wise
//! dedup can't skip the scan either.
//!
//! Run with `cargo bench -p moq-mux --bench parameter_sets`.

use moq_mux::codec::h264::Split;

const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde];
const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21];

/// A PPS defining `id`, as ue(v), followed by a 16-bit `tag` that keeps its bytes distinct.
fn pps(id: u32, tag: u16) -> Vec<u8> {
	let code = id + 1;
	let len = 32 - code.leading_zeros();
	let mut bits: Vec<bool> = std::iter::repeat_n(false, len as usize - 1).collect();
	bits.extend((0..len).rev().map(|bit| code >> bit & 1 == 1));
	// An always-set bit before each tag byte keeps any two bytes from both being zero.
	for byte in tag.to_be_bytes() {
		bits.push(true);
		bits.extend((0..8).rev().map(|bit| byte >> bit & 1 == 1));
	}
	bits.push(true); // rbsp_stop_one_bit
	let mut out = vec![0x68];
	for chunk in bits.chunks(8) {
		out.push(
			chunk
				.iter()
				.enumerate()
				.fold(0, |byte, (i, &bit)| byte | u8::from(bit) << (7 - i)),
		);
	}
	out
}

fn keyframe(count: u32, distinct: bool) -> Vec<u8> {
	let mut nals = vec![SPS.to_vec()];
	nals.extend((0..count).map(|n| pps(if distinct { n } else { 0 }, n as u16)));
	nals.push(IDR.to_vec());
	nals.into_iter()
		.flat_map(|nal| [0, 0, 0, 1].into_iter().chain(nal))
		.collect()
}

const COUNTS: [u32; 4] = [1, 16, 256, 4096];

/// Splitting the keyframe into frames, flushed.
fn split(data: &[u8]) -> Vec<moq_mux::container::Frame> {
	let pts = moq_net::Timestamp::from_micros(0).unwrap();
	let mut split = Split::new();
	let mut frames = split.decode(data, pts).unwrap();
	frames.extend(split.flush(pts).unwrap());
	frames
}

mod h264_split_pps {
	use swole::bench;

	use super::{COUNTS, keyframe};

	fn distinct_keyframe(count: u32) -> Vec<u8> {
		keyframe(count, true)
	}

	#[bench(args = COUNTS, setup = distinct_keyframe, items = |count| count)]
	fn distinct(data: &[u8]) -> Vec<moq_mux::container::Frame> {
		super::split(data)
	}

	fn repeated_keyframe(count: u32) -> Vec<u8> {
		keyframe(count, false)
	}

	#[bench(args = COUNTS, setup = repeated_keyframe, items = |count| count)]
	fn repeated(data: &[u8]) -> Vec<moq_mux::container::Frame> {
		super::split(data)
	}
}

fn main() {
	swole::main();
}
