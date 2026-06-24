//! Per-frame DEFLATE compression for the JSON frame stream.
//!
//! Each frame payload is compressed on its own as a raw DEFLATE ([RFC 1951]) blob, the same
//! format the browser's `CompressionStream("deflate-raw")` produces and consumes. That keeps a
//! Rust producer and a browser (`@moq/json`) consumer interoperable on the wire, at the cost of
//! no cross-frame context: each frame compresses in isolation, so snapshots and large frames
//! shrink well while tiny deltas barely benefit.
//!
//! [RFC 1951]: https://www.rfc-editor.org/rfc/rfc1951.html

use std::io::{Read, Write};

use bytes::Bytes;
use flate2::Compression as Level;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

use crate::{Error, Result};

/// Default DEFLATE level: zlib's own default, a good size/speed balance for the small,
/// repetitive payloads this targets.
const DEFAULT_LEVEL: u32 = 6;

/// Maximum decompressed size of a single frame.
///
/// A malicious publisher could otherwise send a tiny slice that inflates hugely, so
/// [`decompress`] stops and returns [`Error::TooLarge`] rather than allocating without limit.
const MAX_DECOMPRESSED_FRAME: u64 = 64 * 1024 * 1024;

/// Scratch buffer size for the streaming decompress loop.
const CHUNK: usize = 8 * 1024;

/// DEFLATE compression settings for a JSON track.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive). Only the producer needs these; decompression is self-describing, so a
/// [`Consumer`](crate::Consumer) is told via [`Consumer::with_compression`](crate::Consumer::with_compression)
/// only *that* a track is compressed, not how.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Compression {
	/// DEFLATE level, `0..=9`. Higher is smaller but slower. Defaults to `6`.
	///
	/// This is a sender-only choice and need not match the consumer (the wire format is the same
	/// at any level). Browser producers can't set it; the platform deflate picks its own level.
	pub level: u32,
}

impl Default for Compression {
	fn default() -> Self {
		Self { level: DEFAULT_LEVEL }
	}
}

impl Compression {
	/// Compress one frame `payload` into a standalone raw DEFLATE blob.
	///
	/// An empty payload yields an empty slice (compressing nothing).
	pub(crate) fn compress(&self, payload: &[u8]) -> Bytes {
		if payload.is_empty() {
			return Bytes::new();
		}

		let mut encoder = DeflateEncoder::new(Vec::with_capacity(payload.len() / 2 + 16), Level::new(self.level));
		encoder.write_all(payload).expect("deflate write");
		Bytes::from(encoder.finish().expect("deflate finish"))
	}
}

/// Decompress one frame `slice` back into its raw DEFLATE payload.
///
/// An empty slice yields an empty payload. Returns [`Error::TooLarge`] if the frame inflates past
/// the per-frame bound, and [`Error::Decompress`] on malformed or truncated input.
pub(crate) fn decompress(slice: &[u8]) -> Result<Bytes> {
	if slice.is_empty() {
		return Ok(Bytes::new());
	}

	let mut decoder = DeflateDecoder::new(slice);
	let mut out = Vec::with_capacity(slice.len() * 2 + 16);
	let mut tmp = [0u8; CHUNK];

	loop {
		let n = decoder.read(&mut tmp).map_err(|_| Error::Decompress)?;
		if n == 0 {
			break;
		}
		if out.len() as u64 + n as u64 > MAX_DECOMPRESSED_FRAME {
			return Err(Error::TooLarge(MAX_DECOMPRESSED_FRAME));
		}
		out.extend_from_slice(&tmp[..n]);
	}

	Ok(Bytes::from(out))
}

#[cfg(test)]
mod test {
	use super::*;

	/// Round-trip a sequence of frames, each compressed and decompressed independently.
	fn roundtrip(config: &Compression, frames: &[&[u8]]) -> Vec<Vec<u8>> {
		frames
			.iter()
			.map(|f| decompress(&config.compress(f)).unwrap().to_vec())
			.collect()
	}

	#[test]
	fn frame_roundtrip() {
		let frames: &[&[u8]] = &[b"the quick brown fox", b"the quick brown dog", b"the lazy fox"];
		let got = roundtrip(&Compression::default(), frames);
		for (a, b) in frames.iter().zip(&got) {
			assert_eq!(*a, b.as_slice());
		}
	}

	#[test]
	fn empty_frame_roundtrips() {
		assert!(Compression::default().compress(b"").is_empty());
		assert!(decompress(b"").unwrap().is_empty());
	}

	#[test]
	fn repetitive_payload_shrinks() {
		// A payload with lots of internal redundancy compresses well even on its own.
		let config = Compression::default();
		let payload = b"Media over QUIC delivers real-time latency at massive scale.".repeat(6);
		let compressed = config.compress(&payload);
		assert!(
			compressed.len() < payload.len(),
			"compressed {} should beat raw {}",
			compressed.len(),
			payload.len()
		);
		assert_eq!(decompress(&compressed).unwrap(), Bytes::from(payload));
	}

	#[test]
	fn decompress_rejects_garbage() {
		// Random bytes that don't form a valid DEFLATE stream are rejected, not silently truncated.
		assert!(matches!(decompress(&[0xff; 64]), Err(Error::Decompress)));
	}
}
