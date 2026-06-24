//! Group-scoped zstd compression for the JSON frame stream.
//!
//! Within a group the frame payloads form a single zstd stream, flushed at each frame
//! boundary so every frame carries its own slice while later frames reuse the earlier ones as
//! context (a snapshot followed by deltas compresses far better than each frame alone). The
//! [`Encoder`]/[`Decoder`] hold that per-group state; both are recreated at every group
//! boundary.
//!
//! Frames use magicless zstd frames with no content checksum: moq-net's framing already
//! delimits each slice, so the per-frame magic number and checksum would be redundant bytes.
//! An optional shared [dictionary](Compression::dictionary) primes the window so even a
//! group's first frame compresses well.

use bytes::Bytes;
use zstd::stream::raw::{CParameter, DParameter, InBuffer, Operation, OutBuffer};
use zstd::zstd_safe::FrameFormat;

use crate::{Error, Result};

/// Default zstd level: the library default, a good size/speed balance for the small,
/// repetitive payloads this targets.
const DEFAULT_LEVEL: i32 = 3;

/// Maximum cumulative *decompressed* size of a single group, across all its frame payloads.
///
/// Compression is group-scoped, so a malicious publisher could otherwise send tiny slices
/// that each inflate hugely. zstd has no built-in total-output limit for streaming magicless
/// frames, so the [`Decoder`] enforces this bound across the group and returns
/// [`Error::TooLarge`] when exceeded, stopping rather than allocating without limit.
const MAX_DECOMPRESSED_GROUP: u64 = 64 * 1024 * 1024;

/// Upper bound on the zstd decode window (log2 bytes); caps per-frame memory amplification so
/// a tiny input can't force a huge window allocation. 27 (128 MiB) is zstd's normal ceiling.
const WINDOW_LOG_MAX: u32 = 27;

/// Scratch buffer size for the zstd streaming loops.
const CHUNK: usize = 8 * 1024;

/// zstd compression settings for a JSON track.
///
/// Construct from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive). Both ends of a track must agree on the [`dictionary`](Self::dictionary);
/// the level is a sender-only choice and need not match.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Compression {
	/// zstd compression level. Higher is smaller but slower. Defaults to `3`.
	pub level: i32,

	/// An optional shared dictionary that primes the window, so even a group's first frame
	/// compresses against known content. Must be identical on the producer and consumer; how a
	/// consumer obtains it is out of band.
	pub dictionary: Option<Bytes>,
}

impl Default for Compression {
	fn default() -> Self {
		Self {
			level: DEFAULT_LEVEL,
			dictionary: None,
		}
	}
}

impl Compression {
	/// Start a fresh per-group encoder.
	pub(crate) fn encoder(&self) -> Encoder {
		let mut e = match &self.dictionary {
			Some(dict) => zstd::stream::raw::Encoder::with_dictionary(self.level, dict).expect("zstd encoder"),
			None => zstd::stream::raw::Encoder::new(self.level).expect("zstd encoder"),
		};
		// Magicless + no content checksum/size: the slices are delimited by moq-net, so those
		// bytes would only be redundant.
		e.set_parameter(CParameter::Format(FrameFormat::Magicless))
			.expect("zstd format");
		e.set_parameter(CParameter::ChecksumFlag(false)).expect("zstd checksum");
		e.set_parameter(CParameter::ContentSizeFlag(false))
			.expect("zstd content size");
		Encoder(e)
	}

	/// Start a fresh per-group decoder.
	pub(crate) fn decoder(&self) -> Decoder {
		let mut d = match &self.dictionary {
			Some(dict) => zstd::stream::raw::Decoder::with_dictionary(dict).expect("zstd decoder"),
			None => zstd::stream::raw::Decoder::new().expect("zstd decoder"),
		};
		d.set_parameter(DParameter::Format(FrameFormat::Magicless))
			.expect("zstd format");
		d.set_parameter(DParameter::WindowLogMax(WINDOW_LOG_MAX))
			.expect("zstd window");
		Decoder { inner: d, produced: 0 }
	}
}

/// Encodes a group's frame payloads into one shared zstd stream, one slice per frame. Hold one
/// per group; the stream is recreated at each group boundary.
pub(crate) struct Encoder(zstd::stream::raw::Encoder<'static>);

impl Encoder {
	/// Compress the next frame's `payload`, returning its slice of the group stream.
	///
	/// An empty payload contributes nothing and yields an empty slice. Later frames reuse
	/// earlier ones as context, so slices must be produced (and later decoded) in frame order.
	pub(crate) fn frame(&mut self, payload: &[u8]) -> Bytes {
		if payload.is_empty() {
			return Bytes::new();
		}

		let mut out = Vec::with_capacity(payload.len() / 2 + 32);
		let mut tmp = [0u8; CHUNK];
		let mut input = InBuffer::around(payload);

		loop {
			let n = {
				let mut output = OutBuffer::around(&mut tmp);
				self.0.run(&mut input, &mut output).expect("zstd run");
				output.pos()
			};
			out.extend_from_slice(&tmp[..n]);
			if input.pos() == payload.len() {
				break;
			}
		}

		// Flush (retaining the window) so this frame's slice is self-delimited while later
		// frames in the group keep reusing the context.
		loop {
			let (remaining, n) = {
				let mut output = OutBuffer::around(&mut tmp);
				let remaining = self.0.flush(&mut output).expect("zstd flush");
				(remaining, output.pos())
			};
			out.extend_from_slice(&tmp[..n]);
			if remaining == 0 {
				break;
			}
		}

		Bytes::from(out)
	}
}

/// Decodes a group's frame slices back into the original payloads. Hold one per group; feed
/// slices in frame order (each frame builds on the earlier ones).
pub(crate) struct Decoder {
	inner: zstd::stream::raw::Decoder<'static>,
	// Cumulative decompressed bytes this group, for the zip-bomb bound.
	produced: u64,
}

impl Decoder {
	/// Decompress the next frame's `slice` back into its payload.
	///
	/// An empty slice yields an empty payload. Returns [`Error::TooLarge`] if the group's
	/// cumulative decompressed size would exceed the bound, and [`Error::Decompress`] on
	/// malformed input.
	pub(crate) fn frame(&mut self, slice: &[u8]) -> Result<Bytes> {
		if slice.is_empty() {
			return Ok(Bytes::new());
		}

		let mut out = Vec::with_capacity(slice.len() * 2 + 16);
		let mut tmp = [0u8; CHUNK];
		let mut input = InBuffer::around(slice);

		loop {
			let n = {
				let mut output = OutBuffer::around(&mut tmp);
				self.inner.run(&mut input, &mut output).map_err(|_| Error::Decompress)?;
				output.pos()
			};
			out.extend_from_slice(&tmp[..n]);
			if self.produced + out.len() as u64 > MAX_DECOMPRESSED_GROUP {
				return Err(Error::TooLarge(MAX_DECOMPRESSED_GROUP));
			}
			if input.pos() == slice.len() && n == 0 {
				break;
			}
		}

		self.produced += out.len() as u64;
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

		let mut dec = config.decoder();
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
	fn cross_frame_redundancy_shrinks() {
		// A later frame identical to an earlier one compresses to far fewer bytes once the
		// window holds the earlier copy.
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
	fn dictionary_shrinks_first_frame() {
		// A dictionary primes the window, so even the group's first frame compresses against it.
		let payload = br#"{"video":{"renditions":{"video0":{"codec":"avc1.64001f"}}}}"#;
		let plain = Compression::default();
		let primed = Compression {
			dictionary: Some(Bytes::from_static(payload)),
			..Default::default()
		};

		let first_plain = plain.encoder().frame(payload);
		let first_primed = primed.encoder().frame(payload);
		assert!(
			first_primed.len() < first_plain.len(),
			"dictionary frame {} should beat undictionaried {}",
			first_primed.len(),
			first_plain.len()
		);

		// And it still round-trips with the same dictionary on the decode side.
		let mut dec = primed.decoder();
		assert_eq!(dec.frame(&first_primed).unwrap(), Bytes::from_static(payload));
	}

	#[test]
	fn decompress_rejects_garbage() {
		let mut dec = Compression::default().decoder();
		assert!(matches!(dec.frame(b"not a zstd stream at all"), Err(Error::Decompress)));
	}
}
