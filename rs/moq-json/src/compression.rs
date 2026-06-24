//! Group-scoped DEFLATE compression for the JSON frame stream.
//!
//! Within a group the frame payloads form a single raw DEFLATE ([RFC 1951]) stream, sync-flushed
//! at each frame boundary so every frame carries its own self-delimited slice while later frames
//! reuse the earlier ones as context (a snapshot followed by deltas compresses far better than
//! each frame alone). The [`Encoder`]/[`Decoder`] hold that per-group state; both are recreated at
//! every group boundary.
//!
//! This is plain raw DEFLATE with a `Z_SYNC_FLUSH` after each frame, so a browser (`@moq/json`)
//! peer interoperates on the wire using the same primitive (zlib's sync flush). A small slice can
//! still inflate to far more than its own size, so [`Decoder::frame`] bounds each frame's output by
//! its declared length, capped at [`MAX_DECOMPRESSED_FRAME`].
//!
//! A sync flush always ends in the 4-byte empty-block marker `00 00 ff ff`. That marker is fixed,
//! so [`Encoder::frame`] drops it from each slice and [`Decoder::frame`] re-appends it before
//! inflating, saving 4 bytes per frame. This is the same trick [RFC 7692] (permessage-deflate)
//! uses for WebSocket messages.
//!
//! Each slice is prefixed with its decompressed length as a [QUIC varint][RFC 9000] (matching
//! `@moq/net`'s `Varint`). The decoder sizes its output buffer up front and rejects an oversized
//! frame before inflating; a future browser decoder can use it to delimit `DecompressionStream`
//! output, which carries no frame boundary of its own.
//!
//! [RFC 1951]: https://www.rfc-editor.org/rfc/rfc1951.html
//! [RFC 7692]: https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1
//! [RFC 9000]: https://www.rfc-editor.org/rfc/rfc9000.html#section-16

use bytes::Bytes;
use flate2::{Compress, Decompress, FlushCompress, FlushDecompress, Status};

use crate::{Error, Result};

/// Default DEFLATE level: zlib's own default, a good size/speed balance for the small, repetitive
/// payloads this targets.
const DEFAULT_LEVEL: u32 = 6;

/// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// Maximum decompressed size of a single frame.
///
/// A malicious publisher could otherwise send a tiny slice that inflates hugely, so
/// [`Decoder::frame`] stops and returns [`Error::TooLarge`] rather than allocating without limit.
const MAX_DECOMPRESSED_FRAME: u64 = 64 * 1024 * 1024;

/// Scratch buffer size for the streaming (de)compress loops.
const CHUNK: usize = 8 * 1024;

/// Append `v` as a QUIC varint (RFC 9000 §16). `v` must fit in 62 bits, which a frame length always
/// does. Matches `@moq/net`'s `Varint` so the two ends agree on the wire.
fn put_varint(out: &mut Vec<u8>, v: u64) {
	if v <= 0x3f {
		out.push(v as u8);
	} else if v <= 0x3fff {
		out.extend_from_slice(&((v as u16) | 0x4000).to_be_bytes());
	} else if v <= 0x3fff_ffff {
		out.extend_from_slice(&((v as u32) | 0x8000_0000).to_be_bytes());
	} else {
		out.extend_from_slice(&(v | 0xc000_0000_0000_0000).to_be_bytes());
	}
}

/// Read a QUIC varint from the front of `buf`, returning the value and the rest. Errors if `buf` is
/// empty or shorter than the encoded length.
fn get_varint(buf: &[u8]) -> Result<(u64, &[u8])> {
	let first = *buf.first().ok_or(Error::Decompress)?;
	let len = 1usize << (first >> 6);
	if buf.len() < len {
		return Err(Error::Decompress);
	}
	let (head, rest) = buf.split_at(len);
	let value = match len {
		1 => (head[0] & 0x3f) as u64,
		2 => (u16::from_be_bytes([head[0], head[1]]) & 0x3fff) as u64,
		4 => (u32::from_be_bytes([head[0], head[1], head[2], head[3]]) & 0x3fff_ffff) as u64,
		_ => u64::from_be_bytes(head.try_into().expect("8 bytes")) & 0x3fff_ffff_ffff_ffff,
	};
	Ok((value, rest))
}

/// A DEFLATE compression level in the valid `0..=9` range.
///
/// `0` stores without compressing, `9` is smallest but slowest. Construct via [`Level::new`],
/// which clamps out-of-range values, so an invalid level (e.g. `99`) is unrepresentable rather
/// than producing backend-dependent output. The level is a sender-only choice and need not match
/// the consumer.
#[derive(Debug, Clone, Copy)]
pub struct Level(u32);

impl Level {
	/// Wrap a raw level, clamping to the valid `0..=9` range.
	pub fn new(level: u32) -> Self {
		Self(level.min(9))
	}

	/// The raw level, guaranteed to be in `0..=9`.
	pub fn get(self) -> u32 {
		self.0
	}
}

impl Default for Level {
	fn default() -> Self {
		Self(DEFAULT_LEVEL)
	}
}

/// DEFLATE compression settings for a JSON track.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive). Only the producer needs these; decompression is self-describing, so a
/// [`Consumer`](crate::Consumer) is told via [`Consumer::with_compression`](crate::Consumer::with_compression)
/// only *that* a track is compressed, not how.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Compression {
	/// DEFLATE [`Level`]. Higher is smaller but slower. Defaults to `6`.
	///
	/// This is a sender-only choice and need not match the consumer (the wire format is the same
	/// at any level). Browser producers can't set it; the platform deflate picks its own level.
	pub level: Level,
}

impl Compression {
	/// Start a fresh per-group encoder with a cold window.
	pub(crate) fn encoder(&self) -> Encoder {
		// `false`: raw DEFLATE, no zlib header/trailer, matching `deflate-raw` on the browser side.
		Encoder(Compress::new(flate2::Compression::new(self.level.get()), false))
	}
}

/// Encodes a group's frame payloads into one shared DEFLATE stream, one self-delimited slice per
/// frame. Hold one per group; the stream is recreated at each group boundary.
pub(crate) struct Encoder(Compress);

impl Encoder {
	/// Compress the next frame's `payload`, returning its slice of the group stream: a decompressed-
	/// length varint prefix, then the DEFLATE bytes minus the fixed sync-flush marker.
	///
	/// An empty payload contributes nothing and yields an empty slice. Later frames reuse earlier
	/// ones as context, so slices must be produced (and later decoded) in frame order.
	pub(crate) fn frame(&mut self, payload: &[u8]) -> Bytes {
		if payload.is_empty() {
			return Bytes::new();
		}

		let mut out = Vec::with_capacity(payload.len() / 2 + 16);
		// Decompressed-length prefix, so the decoder sizes its buffer and bounds the frame up front.
		put_varint(&mut out, payload.len() as u64);
		let prefix = out.len();
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

		// Drop the fixed sync-flush marker; the decoder re-appends it (see the module docs). It sits
		// after the varint prefix, so there's always a full marker to strip.
		debug_assert!(
			out.len() >= prefix + SYNC_FLUSH_TAIL.len() && out.ends_with(&SYNC_FLUSH_TAIL),
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
	/// An empty slice yields an empty payload. Returns [`Error::TooLarge`] if the declared length
	/// exceeds the per-frame bound, and [`Error::Decompress`] on malformed input or a length that
	/// doesn't match the inflated output.
	pub(crate) fn frame(&mut self, slice: &[u8]) -> Result<Bytes> {
		if slice.is_empty() {
			return Ok(Bytes::new());
		}

		// The decompressed-length prefix bounds and sizes the output before any inflation.
		let (declared, deflate) = get_varint(slice)?;
		if declared > MAX_DECOMPRESSED_FRAME {
			return Err(Error::TooLarge(MAX_DECOMPRESSED_FRAME));
		}
		let mut out = Vec::with_capacity(declared as usize);
		let mut tmp = [0u8; CHUNK];

		// Feed the DEFLATE bytes followed by the re-appended sync-flush marker, which delimits the
		// frame and flushes its last bytes out of the inflate buffer.
		for segment in [deflate, &SYNC_FLUSH_TAIL] {
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

		// The inflated output must match the declared length; a mismatch means a corrupt or lying frame.
		if out.len() as u64 != declared {
			return Err(Error::Decompress);
		}
		Ok(Bytes::from(out))
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// Round-trip a sequence of frames through a group encoder/decoder pair.
	fn roundtrip(config: &Compression, frames: &[&[u8]]) -> Vec<Vec<u8>> {
		let mut enc = config.encoder();
		let slices: Vec<Bytes> = frames.iter().map(|f| enc.frame(f)).collect();

		let mut dec = Decoder::new();
		slices.iter().map(|s| dec.frame(s).unwrap().to_vec()).collect()
	}

	#[test]
	fn group_roundtrip() {
		let frames: &[&[u8]] = &[b"the quick brown fox", b"the quick brown dog", b"the lazy fox"];
		let got = roundtrip(&Compression::default(), frames);
		for (a, b) in frames.iter().zip(&got) {
			assert_eq!(*a, b.as_slice());
		}
	}

	#[test]
	fn empty_frames_roundtrip() {
		let frames: &[&[u8]] = &[b"", b"hello", b"", b"world"];
		let got = roundtrip(&Compression::default(), frames);
		assert_eq!(
			got,
			vec![b"".to_vec(), b"hello".to_vec(), b"".to_vec(), b"world".to_vec()]
		);
	}

	#[test]
	fn large_frame_roundtrips() {
		// A frame larger than the scratch buffer exercises the multi-iteration (de)compress loops.
		let payload = b"abcdefghij".repeat(4096); // 40 KiB, > CHUNK
		let got = roundtrip(&Compression::default(), &[&payload]);
		assert_eq!(got[0], payload);
	}

	#[test]
	fn cross_frame_context_shrinks() {
		// A later frame identical to an earlier one compresses to far fewer bytes once the window
		// holds the earlier copy: this is the whole point of a shared per-group stream.
		let config = Compression::default();
		let payload = b"Media over QUIC delivers real-time latency at massive scale.".repeat(6);
		let mut enc = config.encoder();
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
	fn level_clamps_out_of_range() {
		// An out-of-range level is clamped, not stored verbatim, so it can't reach the backend.
		assert_eq!(Level::new(99).get(), 9);
		assert_eq!(Level::new(6).get(), 6);
		assert_eq!(Level::default().get(), DEFAULT_LEVEL);
	}

	#[test]
	fn decompress_rejects_garbage() {
		let mut dec = Decoder::new();
		assert!(matches!(
			dec.frame(b"not a deflate stream at all"),
			Err(Error::Decompress)
		));
	}

	#[test]
	fn rejects_oversized_declared_length() {
		// A forged prefix claiming more than the cap is rejected before any inflation, so the guard
		// holds without materializing a huge buffer.
		let mut forged = Vec::new();
		put_varint(&mut forged, MAX_DECOMPRESSED_FRAME + 1);
		forged.push(0);
		let mut dec = Decoder::new();
		assert!(matches!(dec.frame(&forged), Err(Error::TooLarge(_))));
	}

	#[test]
	fn rejects_length_mismatch() {
		// A prefix that disagrees with the inflated output (here understated) is rejected as corrupt.
		let mut enc = Compression::default().encoder();
		let slice = enc.frame(b"hello world");
		let (_, deflate) = get_varint(&slice).unwrap();
		let mut tampered = Vec::new();
		put_varint(&mut tampered, 4); // the payload is 11 bytes
		tampered.extend_from_slice(deflate);
		let mut dec = Decoder::new();
		assert!(matches!(dec.frame(&tampered), Err(Error::Decompress)));
	}

	#[test]
	fn varint_round_trips() {
		// Spot-check the QUIC varint boundaries the prefix relies on.
		for v in [0u64, 0x3f, 0x40, 0x3fff, 0x4000, 0x3fff_ffff, MAX_DECOMPRESSED_FRAME] {
			let mut buf = Vec::new();
			put_varint(&mut buf, v);
			let (got, rest) = get_varint(&buf).unwrap();
			assert_eq!(got, v);
			assert!(rest.is_empty());
		}
	}
}
