//! Group-scoped payload compression.
//!
//! A publisher marks a [`crate::Track`] with a [`Compression`] algorithm when its
//! payloads are worth compressing (e.g. a JSON catalog). Within a group the frame
//! payloads form a single compressed stream, flushed at each frame boundary so
//! each frame still carries its own slice while later frames reuse the earlier
//! ones as context. The [`Encoder`]/[`Decoder`] keep that per-group state; both are
//! reset at every group boundary.
//!
//! The algorithm is negotiated per hop (each endpoint advertises the algorithms it
//! can decode in SETUP) and named explicitly on the wire, so a relay can keep
//! payloads compressed in RAM and pass them through to a hop that speaks the same
//! algorithm, only decompressing for hops that don't. The codec here is the shared
//! building block both the wire layer and the model cache use.

use bytes::Bytes;
use flate2::{Compress, Decompress, FlushCompress, FlushDecompress, Status};
use zstd::stream::raw::{CParameter, DParameter, InBuffer, Operation, OutBuffer};
use zstd::zstd_safe::FrameFormat;

use crate::{Error, Result};

/// zstd compression level (the library default; a good size/speed balance for the
/// small, repetitive payloads this targets).
const ZSTD_LEVEL: i32 = 3;

/// Maximum cumulative *decompressed* size of a single group, across all its frame
/// payloads. Compression is group-scoped, so a malicious peer could otherwise send
/// many tiny slices that each inflate hugely; the [`Decoder`] enforces this bound
/// across the group and surfaces [`Error::FrameTooLarge`] when exceeded, so the
/// caller resets the stream rather than allocating without limit.
const MAX_DECOMPRESSED_GROUP: u64 = 64 * 1024 * 1024;

/// Scratch buffer size for the zstd streaming loops.
const ZSTD_CHUNK: usize = 8 * 1024;

/// A payload compression algorithm, negotiated per hop and named on the wire.
///
/// `None`-ness (no compression) is modeled as `Option<Compression>`, so this enum
/// only ever names a real algorithm. `deflate` is the mandatory baseline; `zstd`
/// is optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Compression {
	/// Raw DEFLATE (RFC 1951), no zlib/gzip header. The mandatory baseline, and the
	/// default a publisher gets from a plain "compress this" request: every
	/// compression-capable endpoint implements it, so it's the safest choice.
	#[default]
	Deflate,
	/// Zstandard (RFC 8878), magicless frames with no content checksum.
	Zstd,
}

impl Compression {
	/// The varint algorithm code used on the wire. `0` is reserved for "no
	/// compression" and is never produced here (that's `Option::None`).
	pub fn to_code(self) -> u64 {
		match self {
			Self::Deflate => 1,
			Self::Zstd => 2,
		}
	}

	/// Parse a wire algorithm code, erroring on `0` (none) and unknown codes. The
	/// caller maps `0` to `None` before calling; an unknown code is a peer using an
	/// algorithm we never advertised, i.e. a protocol violation.
	pub fn from_code(code: u64) -> Result<Self> {
		match code {
			1 => Ok(Self::Deflate),
			2 => Ok(Self::Zstd),
			_ => Err(Error::Unsupported),
		}
	}

	/// Start a fresh per-group [`Encoder`] for this algorithm.
	pub fn encoder(self) -> Encoder {
		Encoder(match self {
			Self::Deflate => EncoderInner::Deflate(Compress::new(flate2::Compression::default(), false)),
			Self::Zstd => EncoderInner::Zstd(Box::new(zstd_encoder())),
		})
	}

	/// Start a fresh per-group [`Decoder`] for this algorithm.
	pub fn decoder(self) -> Decoder {
		Decoder {
			inner: match self {
				Self::Deflate => DecoderInner::Deflate(Decompress::new(false)),
				Self::Zstd => DecoderInner::Zstd(Box::new(zstd_decoder())),
			},
			produced: 0,
		}
	}
}

/// Pick the algorithm for a sender-to-receiver direction: the receiver's
/// most-preferred advertised `decoders` that this sender can also `encode`, or
/// `None` if they share nothing. The result is named explicitly on the wire, so
/// the two ends don't need to compute the same value.
pub(crate) fn select(decoders: &[Compression], encoders: &[Compression]) -> Option<Compression> {
	decoders.iter().copied().find(|d| encoders.contains(d))
}

/// Encodes a group's frame payloads into one shared compressed stream, one slice
/// per frame. Hold one per group (the stream is reset at each group boundary).
pub struct Encoder(EncoderInner);

enum EncoderInner {
	Deflate(Compress),
	// Boxed: the zstd context is large and rarely the hot path.
	Zstd(Box<zstd::stream::raw::Encoder<'static>>),
}

impl Encoder {
	/// Compress the next frame's `payload`, returning its slice of the group stream.
	///
	/// An empty payload contributes nothing to the stream and yields an empty slice.
	/// Later frames reuse earlier ones as context, so slices must be produced (and
	/// later decoded) in frame order.
	pub fn frame(&mut self, payload: &[u8]) -> Bytes {
		if payload.is_empty() {
			return Bytes::new();
		}
		match &mut self.0 {
			EncoderInner::Deflate(c) => Bytes::from(deflate_frame(c, payload)),
			EncoderInner::Zstd(e) => Bytes::from(zstd_frame(e, payload)),
		}
	}
}

/// Decodes a group's frame slices back into the original payloads. Hold one per
/// group; feed slices in frame order (each frame builds on the earlier ones).
pub struct Decoder {
	inner: DecoderInner,
	// Cumulative decompressed bytes this group, for the decompression-bomb bound.
	produced: u64,
}

enum DecoderInner {
	Deflate(Decompress),
	Zstd(Box<zstd::stream::raw::Decoder<'static>>),
}

impl Decoder {
	/// Decompress the next frame's `slice` back into its payload.
	///
	/// An empty slice yields an empty payload. Returns [`Error::FrameTooLarge`] if
	/// the group's cumulative decompressed size would exceed the bomb bound, and
	/// [`Error::Decompress`] on malformed input.
	pub fn frame(&mut self, slice: &[u8]) -> Result<Bytes> {
		if slice.is_empty() {
			return Ok(Bytes::new());
		}
		let out = match &mut self.inner {
			DecoderInner::Deflate(d) => deflate_unframe(d, slice, &mut self.produced)?,
			DecoderInner::Zstd(d) => zstd_unframe(d, slice, &mut self.produced)?,
		};
		Ok(Bytes::from(out))
	}
}

/// Ensure the vec has room to receive more output before the next `*_vec` call,
/// which writes into spare capacity rather than growing the vec itself.
fn reserve(out: &mut Vec<u8>) {
	if out.len() == out.capacity() {
		out.reserve(out.capacity().max(64));
	}
}

/// DEFLATE one frame: feed the payload, sync-flush, and strip the redundant
/// trailing `00 00 FF FF` the sync flush emits (RFC 7692); the decoder re-inserts
/// it. The window carries over to the next frame.
fn deflate_frame(c: &mut Compress, payload: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(payload.len() + 16);

	// Feed the payload and sync-flush in one pass. The zlib idiom for "flush done"
	// is `avail_out != 0`: once the call consumes all input *and* leaves spare output
	// room, the flush (including its trailing marker) is complete. A sync flush
	// re-emits its marker only on a fresh call, never mid-flush, so breaking here
	// never double-emits. (Looping `compress_vec(&[], Sync)` instead would re-emit
	// forever; breaking on `Status::Ok` can stop mid-flush, before the marker.)
	let mut consumed = 0;
	loop {
		reserve(&mut out);
		let before = c.total_in();
		c.compress_vec(&payload[consumed..], &mut out, FlushCompress::Sync)
			.expect("deflate compress into vec");
		consumed += (c.total_in() - before) as usize;
		if consumed == payload.len() && out.len() < out.capacity() {
			break;
		}
	}

	// RFC 7692: drop the trailing empty stored block the sync flush emits; the
	// decoder re-inserts it.
	debug_assert!(out.ends_with(&[0, 0, 0xff, 0xff]), "deflate sync flush trailer");
	out.truncate(out.len().saturating_sub(4));
	out
}

/// Inverse of [`deflate_frame`]: re-append the `00 00 FF FF` trailer and inflate.
fn deflate_unframe(d: &mut Decompress, slice: &[u8], produced: &mut u64) -> Result<Vec<u8>> {
	let mut input = Vec::with_capacity(slice.len() + 4);
	input.extend_from_slice(slice);
	input.extend_from_slice(&[0, 0, 0xff, 0xff]);

	let mut out = Vec::with_capacity(slice.len() * 2 + 16);
	let mut consumed = 0;
	loop {
		reserve(&mut out);
		let in_before = d.total_in();
		let out_before = d.total_out();
		let status = d
			.decompress_vec(&input[consumed..], &mut out, FlushDecompress::None)
			.map_err(|_| Error::Decompress)?;
		consumed += (d.total_in() - in_before) as usize;
		if *produced + out.len() as u64 > MAX_DECOMPRESSED_GROUP {
			return Err(Error::FrameTooLarge);
		}
		if matches!(status, Status::StreamEnd) {
			break;
		}
		if consumed == input.len() && d.total_out() == out_before {
			break;
		}
	}

	*produced += out.len() as u64;
	Ok(out)
}

fn zstd_encoder() -> zstd::stream::raw::Encoder<'static> {
	let mut e = zstd::stream::raw::Encoder::new(ZSTD_LEVEL).expect("zstd encoder");
	// Magicless frame + no content checksum: moq-lite delimits the slices itself,
	// so the per-frame magic number and checksum would only be redundant bytes.
	e.set_parameter(CParameter::Format(FrameFormat::Magicless))
		.expect("zstd format");
	e.set_parameter(CParameter::ChecksumFlag(false)).expect("zstd checksum");
	e.set_parameter(CParameter::ContentSizeFlag(false))
		.expect("zstd content size");
	e
}

fn zstd_decoder() -> zstd::stream::raw::Decoder<'static> {
	let mut d = zstd::stream::raw::Decoder::new().expect("zstd decoder");
	d.set_parameter(DParameter::Format(FrameFormat::Magicless))
		.expect("zstd format");
	d
}

/// zstd one frame: feed the payload, then `ZSTD_e_flush` (retains the window) so
/// later frames in the group reuse it.
fn zstd_frame(e: &mut zstd::stream::raw::Encoder<'static>, payload: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(payload.len() / 2 + 32);
	let mut tmp = [0u8; ZSTD_CHUNK];
	let mut input = InBuffer::around(payload);

	loop {
		let n = {
			let mut output = OutBuffer::around(&mut tmp);
			e.run(&mut input, &mut output).expect("zstd run");
			output.pos()
		};
		out.extend_from_slice(&tmp[..n]);
		if input.pos() == payload.len() {
			break;
		}
	}

	loop {
		let (remaining, n) = {
			let mut output = OutBuffer::around(&mut tmp);
			let remaining = e.flush(&mut output).expect("zstd flush");
			(remaining, output.pos())
		};
		out.extend_from_slice(&tmp[..n]);
		if remaining == 0 {
			break;
		}
	}

	out
}

/// Inverse of [`zstd_frame`].
fn zstd_unframe(d: &mut zstd::stream::raw::Decoder<'static>, slice: &[u8], produced: &mut u64) -> Result<Vec<u8>> {
	let mut out = Vec::with_capacity(slice.len() * 2 + 16);
	let mut tmp = [0u8; ZSTD_CHUNK];
	let mut input = InBuffer::around(slice);

	loop {
		let n = {
			let mut output = OutBuffer::around(&mut tmp);
			d.run(&mut input, &mut output).map_err(|_| Error::Decompress)?;
			output.pos()
		};
		out.extend_from_slice(&tmp[..n]);
		if *produced + out.len() as u64 > MAX_DECOMPRESSED_GROUP {
			return Err(Error::FrameTooLarge);
		}
		if input.pos() == slice.len() && n == 0 {
			break;
		}
	}

	*produced += out.len() as u64;
	Ok(out)
}

#[cfg(test)]
mod test {
	use super::*;

	/// Round-trip a sequence of frames through a group encoder/decoder pair.
	fn roundtrip(algo: Compression, frames: &[&[u8]]) -> Vec<Vec<u8>> {
		let mut enc = algo.encoder();
		let slices: Vec<Bytes> = frames.iter().map(|f| enc.frame(f)).collect();

		let mut dec = algo.decoder();
		slices.iter().map(|s| dec.frame(s).unwrap().to_vec()).collect()
	}

	#[test]
	fn deflate_group_roundtrip() {
		let frames: &[&[u8]] = &[b"the quick brown fox", b"the quick brown dog", b"the lazy fox"];
		let got = roundtrip(Compression::Deflate, frames);
		for (a, b) in frames.iter().zip(&got) {
			assert_eq!(*a, b.as_slice());
		}
	}

	#[test]
	fn zstd_group_roundtrip() {
		let frames: &[&[u8]] = &[b"the quick brown fox", b"the quick brown dog", b"the lazy fox"];
		let got = roundtrip(Compression::Zstd, frames);
		for (a, b) in frames.iter().zip(&got) {
			assert_eq!(*a, b.as_slice());
		}
	}

	#[test]
	fn empty_frames_roundtrip() {
		for algo in [Compression::Deflate, Compression::Zstd] {
			let frames: &[&[u8]] = &[b"", b"hello", b"", b"world"];
			let got = roundtrip(algo, frames);
			assert_eq!(
				got,
				vec![b"".to_vec(), b"hello".to_vec(), b"".to_vec(), b"world".to_vec()]
			);
		}
	}

	#[test]
	fn cross_frame_redundancy_shrinks() {
		// A later frame identical to an earlier one should compress to far fewer
		// bytes once the window holds the earlier copy. (Use varied text, not a
		// single repeated byte, which already compresses to a tiny run on its own.)
		for algo in [Compression::Deflate, Compression::Zstd] {
			let payload = b"Media over QUIC delivers real-time latency at massive scale.".repeat(6);
			let mut enc = algo.encoder();
			let first = enc.frame(&payload);
			let second = enc.frame(&payload);
			assert!(
				second.len() < first.len(),
				"{algo:?}: repeat frame {} should be smaller than first {}",
				second.len(),
				first.len()
			);
		}
	}

	#[test]
	fn deflate_shrinks_repetitive() {
		let mut enc = Compression::Deflate.encoder();
		let data = vec![b'a'; 4096];
		assert!(enc.frame(&data).len() < data.len());
	}

	#[test]
	fn decompress_rejects_garbage() {
		let mut dec = Compression::Deflate.decoder();
		assert!(matches!(
			dec.frame(b"not a deflate stream at all"),
			Err(Error::Decompress)
		));
	}

	#[test]
	fn code_roundtrip() {
		for c in [Compression::Deflate, Compression::Zstd] {
			assert_eq!(Compression::from_code(c.to_code()).unwrap(), c);
		}
		assert!(Compression::from_code(0).is_err());
		assert!(Compression::from_code(99).is_err());
	}

	#[test]
	fn select_prefers_receiver_order() {
		// Receiver prefers zstd; sender can encode both -> zstd.
		assert_eq!(
			select(
				&[Compression::Zstd, Compression::Deflate],
				&[Compression::Deflate, Compression::Zstd]
			),
			Some(Compression::Zstd)
		);
		// Receiver only decodes deflate -> deflate even if sender prefers zstd.
		assert_eq!(
			select(&[Compression::Deflate], &[Compression::Zstd, Compression::Deflate]),
			Some(Compression::Deflate)
		);
		// No overlap -> None.
		assert_eq!(select(&[Compression::Zstd], &[Compression::Deflate]), None);
		assert_eq!(select(&[], &[Compression::Deflate]), None);
	}
}
