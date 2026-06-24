//! Group-scoped DEFLATE compression for the JSON frame stream.
//!
//! Within a group the frame payloads form a single raw DEFLATE ([RFC 1951]) stream, sync-flushed
//! at each frame boundary so every frame carries its own self-delimited slice while later frames
//! reuse the earlier ones as context (a snapshot followed by deltas compresses far better than
//! each frame alone). The [`Encoder`]/[`Decoder`] hold that per-group state; both are recreated at
//! every group boundary.
//!
//! This is plain raw DEFLATE with a `Z_SYNC_FLUSH` after each frame, so a browser (`@moq/json`)
//! peer interoperates on the wire using the same primitive (zlib's sync flush). moq-net already
//! frames each slice, so there's no length prefix. A small slice can still inflate to far more than
//! its own size, so [`Decoder::frame`] bounds each frame's output at [`MAX_DECOMPRESSED_FRAME`].
//!
//! A sync flush always ends in the 4-byte empty-block marker `00 00 ff ff`. That marker is fixed,
//! so [`Encoder::frame`] drops it from each slice and [`Decoder::frame`] re-appends it before
//! inflating, saving 4 bytes per frame. This is the same trick [RFC 7692] (permessage-deflate)
//! uses for WebSocket messages.
//!
//! [RFC 1951]: https://www.rfc-editor.org/rfc/rfc1951.html
//! [RFC 7692]: https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1

use bytes::Bytes;
use flate2::{Compress, Decompress, FlushCompress, FlushDecompress, Status};

use crate::{Error, Result};

/// DEFLATE level for the frame stream: zlib's own default, a good size/speed balance for the small,
/// repetitive payloads this targets.
const LEVEL: u32 = 6;

/// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// Maximum decompressed size of a single frame.
///
/// A malicious publisher could otherwise send a tiny slice that inflates hugely, so
/// [`Decoder::frame`] stops and returns [`Error::TooLarge`] rather than allocating without limit.
const MAX_DECOMPRESSED_FRAME: u64 = 64 * 1024 * 1024;

/// Scratch buffer size for the streaming (de)compress loops.
const CHUNK: usize = 8 * 1024;

/// Encodes a group's frame payloads into one shared DEFLATE stream, one self-delimited slice per
/// frame. Hold one per group; the stream is recreated at each group boundary.
pub(crate) struct Encoder(Compress);

impl Encoder {
	/// Start a fresh per-group encoder with a cold window.
	pub(crate) fn new() -> Self {
		// `false`: raw DEFLATE, no zlib header/trailer, matching `deflate-raw` on the browser side.
		Self(Compress::new(flate2::Compression::new(LEVEL), false))
	}

	/// Compress the next frame's `payload`, returning its slice of the group stream: the DEFLATE
	/// bytes minus the fixed sync-flush marker. Empty in yields empty out. Later frames reuse earlier
	/// ones as context, so slices must be produced (and later decoded) in frame order.
	pub(crate) fn frame(&mut self, payload: &[u8]) -> Bytes {
		if payload.is_empty() {
			return Bytes::new();
		}

		let mut out = Vec::with_capacity(payload.len() / 2 + 16);
		let mut tmp = [0u8; CHUNK];
		let mut input = payload;

		// Drive the stream with a sync flush so this frame's slice is self-delimited (byte-aligned,
		// window retained). The classic zlib loop: keep going while the output buffer fills up.
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

		// Drop the fixed sync-flush marker; the decoder re-appends it (see the module docs).
		debug_assert!(
			out.ends_with(&SYNC_FLUSH_TAIL),
			"a sync flush must end in the deflate marker"
		);
		out.truncate(out.len() - SYNC_FLUSH_TAIL.len());
		Bytes::from(out)
	}
}

/// Decodes a group's frame slices back into the original payloads. Hold one per group; feed slices
/// in frame order (each frame builds on the earlier ones).
pub(crate) struct Decoder(Decompress);

impl Decoder {
	/// Start a fresh per-group decoder with a cold window.
	pub(crate) fn new() -> Self {
		// `false`: raw DEFLATE, matching the encoder.
		Self(Decompress::new(false))
	}

	/// Decompress the next frame's `slice` back into its payload.
	///
	/// An empty slice yields an empty payload. Returns [`Error::TooLarge`] if the frame inflates past
	/// the per-frame bound (checked as output is produced, not from any declared size), and
	/// [`Error::Decompress`] on malformed input.
	pub(crate) fn frame(&mut self, slice: &[u8]) -> Result<Bytes> {
		if slice.is_empty() {
			return Ok(Bytes::new());
		}

		let mut out = Vec::new();
		let mut tmp = [0u8; CHUNK];

		// Feed the wire slice followed by the re-appended sync-flush marker, which delimits the frame
		// and flushes its last bytes out of the inflate buffer.
		for segment in [slice, &SYNC_FLUSH_TAIL] {
			let mut input = segment;
			loop {
				let before_in = self.0.total_in();
				let before_out = self.0.total_out();
				let status = self
					.0
					.decompress(input, &mut tmp, FlushDecompress::Sync)
					.map_err(|_| Error::Decompress)?;
				let consumed = (self.0.total_in() - before_in) as usize;
				let produced = (self.0.total_out() - before_out) as usize;
				// Bound the inflated output as it is produced; a tiny slice can expand enormously.
				if out.len() as u64 + produced as u64 > MAX_DECOMPRESSED_FRAME {
					return Err(Error::TooLarge(MAX_DECOMPRESSED_FRAME));
				}
				out.extend_from_slice(&tmp[..produced]);
				input = &input[consumed..];

				// Move to the next segment once this one is drained and the buffer wasn't saturated. The
				// no-progress guard avoids spinning when the marker needs no further output.
				if matches!(status, Status::StreamEnd) || (input.is_empty() && produced < tmp.len()) {
					break;
				}
				if consumed == 0 && produced == 0 {
					break;
				}
			}
		}

		Ok(Bytes::from(out))
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Round-trip a sequence of frames through a group encoder/decoder pair.
	fn roundtrip(frames: &[&[u8]]) -> Vec<Vec<u8>> {
		let mut enc = Encoder::new();
		let slices: Vec<Bytes> = frames.iter().map(|f| enc.frame(f)).collect();

		let mut dec = Decoder::new();
		slices.iter().map(|s| dec.frame(s).unwrap().to_vec()).collect()
	}

	#[test]
	fn group_roundtrip() {
		let frames: &[&[u8]] = &[b"the quick brown fox", b"the quick brown dog", b"the lazy fox"];
		let got = roundtrip(frames);
		for (a, b) in frames.iter().zip(&got) {
			assert_eq!(*a, b.as_slice());
		}
	}

	#[test]
	fn empty_frames_roundtrip() {
		assert!(Encoder::new().frame(b"").is_empty());
		assert!(Decoder::new().frame(b"").unwrap().is_empty());
	}

	#[test]
	fn cross_frame_context_shrinks() {
		// A later frame identical to an earlier one compresses to far fewer bytes once the window
		// holds the earlier copy: this is the whole point of a shared per-group stream.
		let payload = b"Media over QUIC delivers real-time latency at massive scale.".repeat(6);
		let mut enc = Encoder::new();
		let first = enc.frame(&payload);
		let second = enc.frame(&payload);
		assert!(
			second.len() < first.len(),
			"repeat frame {} should be smaller than first {}",
			second.len(),
			first.len()
		);
	}

	#[test]
	fn frame_larger_than_chunk_roundtrips() {
		// High-entropy data barely compresses, so its slice exceeds the streaming `CHUNK` scratch
		// buffer and the (de)compress loops must iterate. Verify it still round-trips byte for byte.
		let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
		let payload: Vec<u8> = (0..64 * 1024)
			.map(|_| {
				state ^= state << 13;
				state ^= state >> 7;
				state ^= state << 17;
				(state >> 56) as u8
			})
			.collect();

		let mut enc = Encoder::new();
		let slice = enc.frame(&payload);
		assert!(slice.len() > CHUNK, "slice {} should exceed CHUNK {CHUNK}", slice.len());

		let mut dec = Decoder::new();
		assert_eq!(dec.frame(&slice).unwrap(), Bytes::from(payload));
	}

	#[test]
	fn decompress_rejects_garbage() {
		let mut dec = Decoder::new();
		assert!(matches!(
			dec.frame(b"not a deflate stream at all"),
			Err(Error::Decompress)
		));
	}
}
