//! The DEFLATE codec behind compressed tracks ([`crate::track::Info::compress`]).
//!
//! Each group is one raw DEFLATE stream ([RFC 1951]) with a sync flush after every frame, so
//! each frame's slice is self-delimited while later frames compress against earlier ones.
//! A sync flush always ends in the marker `00 00 ff ff`, which [`Encoder::frame`] drops and
//! [`Decoder::frame`] re-appends, the same trick as [RFC 7692]. The format is
//! draft-lcurley-moq-flate.
//!
//! [RFC 1951]: https://www.rfc-editor.org/rfc/rfc1951.html
//! [RFC 7692]: https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1

use bytes::Bytes;
use flate2::{Compress, Decompress, FlushCompress, FlushDecompress, Status};

use crate::{Error, Result};

/// zlib's default level: a good size/speed balance for small, repetitive payloads.
const LEVEL: u32 = 6;

/// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// Scratch buffer size for the streaming (de)compress loops.
const CHUNK: usize = 8 * 1024;

/// The DEFLATE window, 2^15 bytes for raw DEFLATE at the default `windowBits`.
const WINDOW: u64 = 1 << 15;

/// Heap an [`Encoder`] holds, as zlib sizes it at the default `memLevel` of 8: the doubled
/// window, its `prev` chain and hash `head` tables, and the pending buffer, 64 KiB each.
pub(super) const ENCODER_HEAP: u64 = 8 * WINDOW;

/// Heap a [`Decoder`] holds: the window, plus its inflate state rounded up to 8 KiB.
pub(super) const DECODER_HEAP: u64 = WINDOW + 8 * 1024;

/// Compresses a group's frames into one shared window, one slice per frame.
// Boxed: zlib-rs keeps its stream header inline, which would bloat the group state.
pub(super) struct Encoder(Box<Compress>);

impl Encoder {
	/// A fresh encoder with a cold window.
	pub(super) fn new() -> Self {
		// `false`: raw DEFLATE, no zlib header/trailer, matching `deflate-raw` in browsers.
		Self(Box::new(Compress::new(flate2::Compression::new(LEVEL), false)))
	}

	/// Compress the next frame, returning its slice minus the sync-flush marker. Empty in
	/// yields empty out, leaving the window untouched.
	pub(super) fn frame(&mut self, payload: &[u8]) -> Bytes {
		if payload.is_empty() {
			return Bytes::new();
		}

		let mut out = Vec::with_capacity(payload.len() / 2 + 16);
		let mut tmp = [0u8; CHUNK];
		let mut input = payload;

		// Keep going while the output buffer fills up: the classic zlib loop.
		loop {
			let before_in = self.0.total_in();
			let before_out = self.0.total_out();
			self.0.compress(input, &mut tmp, FlushCompress::Sync).expect("deflate");
			let consumed = (self.0.total_in() - before_in) as usize;
			let produced = (self.0.total_out() - before_out) as usize;
			out.extend_from_slice(&tmp[..produced]);
			input = &input[consumed..];
			if produced < tmp.len() {
				break;
			}
		}

		// A missing marker means the backend returned before finishing the flush; panic
		// rather than emit a truncated frame.
		assert!(
			out.ends_with(&SYNC_FLUSH_TAIL),
			"a sync flush must end in the deflate marker"
		);
		out.truncate(out.len() - SYNC_FLUSH_TAIL.len());
		Bytes::from(out)
	}
}

/// Inflates a group's slices back into frames, in order.
pub(super) struct Decoder(Box<Decompress>);

impl Decoder {
	/// A fresh decoder with a cold window.
	pub(super) fn new() -> Self {
		Self(Box::new(Decompress::new(false)))
	}

	/// Inflate the next frame's `slice`, which must produce exactly `size` bytes.
	///
	/// Output grows as it is produced and stops at `size`, so a slice that inflates past
	/// its declaration costs no more than the declaration. [`Error::WrongSize`] if the
	/// output differs from `size`, [`Error::MalformedTrack`] if the slice is not DEFLATE or
	/// ends the stream, which a group never does.
	pub(super) fn frame(&mut self, slice: &[u8], size: usize) -> Result<Bytes> {
		if slice.is_empty() {
			return match size {
				0 => Ok(Bytes::new()),
				_ => Err(Error::WrongSize),
			};
		}

		let mut out = Vec::new();
		let mut tmp = [0u8; CHUNK];

		// Feed the slice followed by the re-appended marker, which ends the frame and
		// flushes its last bytes out of the inflate buffer.
		for segment in [slice, &SYNC_FLUSH_TAIL] {
			let mut input = segment;
			loop {
				let before_in = self.0.total_in();
				let before_out = self.0.total_out();
				let status = self
					.0
					.decompress(input, &mut tmp, FlushDecompress::Sync)
					.map_err(|_| Error::MalformedTrack)?;
				// A final block would let trailing bytes and every later frame go unread.
				if matches!(status, Status::StreamEnd) {
					return Err(Error::MalformedTrack);
				}
				let consumed = (self.0.total_in() - before_in) as usize;
				let produced = (self.0.total_out() - before_out) as usize;
				if out.len() + produced > size {
					return Err(Error::WrongSize);
				}
				out.extend_from_slice(&tmp[..produced]);
				input = &input[consumed..];

				if input.is_empty() && produced < tmp.len() {
					break;
				}
				// No progress: the marker needed no further output.
				if consumed == 0 && produced == 0 {
					break;
				}
			}
		}

		if out.len() != size {
			return Err(Error::WrongSize);
		}
		Ok(Bytes::from(out))
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Deterministic high-entropy bytes: they barely compress, so slices outgrow `CHUNK`.
	fn noise(len: usize) -> Vec<u8> {
		let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
		(0..len)
			.map(|_| {
				state ^= state << 13;
				state ^= state >> 7;
				state ^= state << 17;
				(state >> 56) as u8
			})
			.collect()
	}

	#[test]
	fn frames_roundtrip_against_a_shared_window() {
		let frames: &[&[u8]] = &[b"the quick brown fox", b"", b"the quick brown dog", b"the lazy fox"];
		let mut enc = Encoder::new();
		let slices: Vec<Bytes> = frames.iter().map(|f| enc.frame(f)).collect();
		let mut dec = Decoder::new();
		for (frame, slice) in frames.iter().zip(&slices) {
			assert_eq!(dec.frame(slice, frame.len()).unwrap(), *frame);
		}
	}

	#[test]
	fn a_repeat_frame_reuses_the_window() {
		let payload = b"Media over QUIC delivers real-time latency at massive scale.".repeat(6);
		let mut enc = Encoder::new();
		let first = enc.frame(&payload);
		let second = enc.frame(&payload);
		assert!(second.len() < first.len());
	}

	/// High-entropy frames straddling a block boundary still round-trip byte for byte, and
	/// their slices outgrow the scratch buffer so both loops iterate.
	#[test]
	fn incompressible_frames_roundtrip() {
		let lens: Vec<usize> = (31 * 1024..32 * 1024 + 256).step_by(64).collect();
		let noise = noise(lens.iter().sum());
		let mut enc = Encoder::new();
		let mut dec = Decoder::new();
		let mut rest = noise.as_slice();
		for len in lens {
			let (frame, next) = rest.split_at(len);
			rest = next;
			let slice = enc.frame(frame);
			assert!(slice.len() > CHUNK);
			assert!(dec.frame(&slice, len).unwrap() == frame, "{len} byte frame corrupted");
		}
	}

	/// The declared size bounds the output both ways.
	#[test]
	fn the_declared_size_must_match() {
		let payload = vec![0u8; 1024];
		let slice = Encoder::new().frame(&payload);
		assert!(matches!(Decoder::new().frame(&slice, 512), Err(Error::WrongSize)));
		assert!(matches!(Decoder::new().frame(&slice, 2048), Err(Error::WrongSize)));
		assert!(matches!(Decoder::new().frame(b"", 1), Err(Error::WrongSize)));
	}

	/// A final block is malformed even when it inflates to the declared size.
	#[test]
	fn a_final_block_is_malformed() {
		let payload = b"the quick brown fox";
		let mut compress = Compress::new(flate2::Compression::new(LEVEL), false);
		let mut slice = Vec::with_capacity(64);
		let status = compress
			.compress_vec(payload, &mut slice, FlushCompress::Finish)
			.unwrap();
		assert!(matches!(status, Status::StreamEnd));
		assert!(matches!(
			Decoder::new().frame(&slice, payload.len()),
			Err(Error::MalformedTrack)
		));
	}

	#[test]
	fn garbage_is_malformed() {
		assert!(matches!(
			Decoder::new().frame(b"not a deflate stream at all", 64),
			Err(Error::MalformedTrack)
		));
	}
}
