//! H.264 Annex-B stream splitter.
//!
//! [`Split`] turns a raw H.264 Annex-B byte stream (inline SPS/PPS, the "avc3"
//! wire shape) into [`crate::container::Frame`]s. It finds access-unit
//! boundaries, caches SPS/PPS and re-inserts them ahead of each keyframe so
//! every keyframe is self-contained, and stamps wall-clock timestamps when the
//! caller has none (stdin).
//!
//! It is deliberately dumb: framing and structural parsing only. It owns no
//! track, catalog, or codec config (no [`VideoConfig`](hang::catalog::VideoConfig)).
//! The importer parses the codec config out of the frames it emits.
//!
//! avc1 (length-prefixed NALU + out-of-band avcC) is not a stream and has no
//! splitter; wrap one access unit with `super::avc1_frame`.

use bytes::{Bytes, BytesMut};

use super::Error;
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};

/// H.264 Annex-B stream splitter: bytes in, [`Frame`](crate::container::Frame)s out.
///
/// Feed bytes via [`decode`](Self::decode) (unknown frame boundaries, e.g.
/// stdin); call [`flush`](Self::flush) to emit the final in-flight access unit.
/// SPS/PPS seen inline are cached and re-inserted ahead of each keyframe so each
/// keyframe is self-contained.
pub struct Split {
	/// Bytes carried over between calls: complete NALs are parsed out on each
	/// [`decode`](Self::decode), leaving the in-flight (final, not-yet-terminated)
	/// NAL here until the next start code arrives or [`flush`](Self::flush) drains it.
	tail: BytesMut,
	current: Avc3Frame,
	sps: Option<Bytes>,
	pps: Option<Bytes>,
	zero: Option<tokio::time::Instant>,
	pending: Vec<crate::container::Frame>,
}

#[derive(Default)]
struct Avc3Frame {
	chunks: BytesMut,
	contains_idr: bool,
	contains_slice: bool,
	contains_sps: bool,
	contains_pps: bool,
}

impl Default for Split {
	fn default() -> Self {
		Self::new()
	}
}

impl Split {
	/// A fresh splitter with an empty parameter-set cache.
	pub fn new() -> Self {
		Self {
			tail: BytesMut::new(),
			current: Avc3Frame::default(),
			sps: None,
			pps: None,
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Decode a buffer where frame boundaries are unknown, returning the access
	/// units it can complete. The leading start code of the *next* access unit is
	/// what signals the previous one is complete, so the final NAL of the in-flight
	/// access unit stays buffered until the next call (or [`flush`](Self::flush)).
	/// The buffer is fully consumed.
	pub fn decode(
		&mut self,
		data: &[u8],
		pts: impl Into<Option<moq_net::Timestamp>>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts.into())?;
		self.tail.extend_from_slice(data);
		// Iterate complete NALs out of `tail`, leaving the trailing (in-flight) NAL
		// (with its start code) buffered for the next call or `flush`.
		let nals = NalIterator::new(&mut self.tail);
		let mut parsed = Vec::new();
		for nal in nals {
			parsed.push(nal?);
		}
		for nal in parsed {
			self.decode_nal(nal, pts)?;
		}
		Ok(std::mem::take(&mut self.pending))
	}

	/// Emit the in-flight access unit, if any. Call after the last
	/// [`decode`](Self::decode) when a caller handed over a complete access unit
	/// (or at end of stream) so the final NAL isn't left buffered.
	pub fn flush(&mut self, pts: impl Into<Option<moq_net::Timestamp>>) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts.into())?;
		if let Some(nal) = NalIterator::new(&mut self.tail).flush()? {
			self.decode_nal(nal, pts)?;
		}
		self.tail.clear();
		self.maybe_start_frame(pts)?;
		Ok(std::mem::take(&mut self.pending))
	}

	fn decode_nal(&mut self, nal: Bytes, pts: moq_net::Timestamp) -> Result<()> {
		let header = nal.first().ok_or(Error::NalTooShort)?;
		let forbidden_zero_bit = (header >> 7) & 1;
		if forbidden_zero_bit != 0 {
			return Err(Error::ForbiddenZeroBit.into());
		}

		let nal_unit_type = header & 0b11111;
		let nal_type = Avc3NalType::try_from(nal_unit_type).ok();

		match nal_type {
			Some(Avc3NalType::Sps) => {
				self.maybe_start_frame(pts)?;
				if self.sps.as_ref().is_some_and(|cached| cached != &nal) {
					// SPS changed mid-stream. The cached PPS is tied to the old
					// SPS and may already have been appended to current.chunks
					// earlier in this AU; reset so the new SPS+PPS pair is the
					// only parameter set we emit.
					self.pps = None;
					self.current.chunks.clear();
					self.current.contains_pps = false;
					self.current.contains_sps = false;
				}
				self.sps = Some(nal.clone());
				self.current.contains_sps = true;
			}
			Some(Avc3NalType::Pps) => {
				self.maybe_start_frame(pts)?;
				self.pps = Some(nal.clone());
				self.current.contains_pps = true;
			}
			Some(Avc3NalType::Aud) | Some(Avc3NalType::Sei) => {
				self.maybe_start_frame(pts)?;
			}
			Some(Avc3NalType::IdrSlice) => {
				if !self.current.contains_sps
					&& let Some(sps) = self.sps.clone()
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(&sps);
					self.current.contains_sps = true;
				}
				if !self.current.contains_pps
					&& let Some(pps) = self.pps.clone()
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(&pps);
					self.current.contains_pps = true;
				}
				self.current.contains_idr = true;
				self.current.contains_slice = true;
			}
			Some(Avc3NalType::NonIdrSlice)
			| Some(Avc3NalType::DataPartitionA)
			| Some(Avc3NalType::DataPartitionB)
			| Some(Avc3NalType::DataPartitionC) => {
				if nal.get(1).ok_or(Error::NalTooShort)? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				self.current.contains_slice = true;
			}
			_ => {}
		}

		tracing::trace!(kind = ?nal_type, "parsed NAL");

		self.current.chunks.extend_from_slice(&START_CODE);
		self.current.chunks.extend_from_slice(&nal);
		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: moq_net::Timestamp) -> Result<()> {
		if !self.current.contains_slice {
			return Ok(());
		}
		let payload = std::mem::take(&mut self.current.chunks).freeze();
		let keyframe = self.current.contains_idr;
		self.current.contains_idr = false;
		self.current.contains_slice = false;
		self.current.contains_sps = false;
		self.current.contains_pps = false;

		self.pending.push(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe,
			duration: None,
		});
		Ok(())
	}

	/// Drop any in-flight access unit.
	///
	/// Pre-reset NALs would otherwise leak into a later frame with the wrong
	/// timestamp. The parameter-set cache is kept so subsequent keyframes stay
	/// self-contained.
	pub fn reset(&mut self) {
		self.current = Avc3Frame::default();
		self.tail.clear();
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}
		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_enum::TryFromPrimitive)]
#[repr(u8)]
enum Avc3NalType {
	Unspecified = 0,
	NonIdrSlice = 1,
	DataPartitionA = 2,
	DataPartitionB = 3,
	DataPartitionC = 4,
	IdrSlice = 5,
	Sei = 6,
	Sps = 7,
	Pps = 8,
	Aud = 9,
	EndOfSeq = 10,
	EndOfStream = 11,
	Filler = 12,
	SpsExt = 13,
	Prefix = 14,
	SubsetSps = 15,
	DepthParameterSet = 16,
}

#[cfg(test)]
mod tests {
	use super::*;

	const SC4: &[u8] = &[0, 0, 0, 1];

	fn annexb(nals: &[&[u8]]) -> BytesMut {
		let mut buf = BytesMut::new();
		for nal in nals {
			buf.extend_from_slice(SC4);
			buf.extend_from_slice(nal);
		}
		buf
	}

	fn ts() -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(0).unwrap()
	}

	/// Decode one complete access unit handed over as a single buffer: `decode`
	/// buffers it, `flush` emits it.
	fn decode_one(split: &mut Split, buf: &mut BytesMut, pts: moq_net::Timestamp) -> Vec<crate::container::Frame> {
		let mut frames = split.decode(buf, pts).unwrap();
		frames.extend(split.flush(pts).unwrap());
		frames
	}

	/// A keyframe access unit fed as one buffer emits one self-contained frame:
	/// SPS+PPS are packaged ahead of the IDR slice and `keyframe` is set.
	#[tokio::test(start_paused = true)]
	async fn decode_packages_keyframe() {
		let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

		let mut split = Split::new();
		let frames = decode_one(&mut split, &mut annexb(&[sps, pps, idr]), ts());

		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		// The payload carries SPS, PPS, then the IDR slice (each start-code prefixed).
		assert_eq!(&frames[0].payload[..SC4.len()], SC4);
		assert!(frames[0].payload.windows(sps.len()).any(|w| w == sps));
		assert!(frames[0].payload.windows(idr.len()).any(|w| w == idr));
	}

	/// Parameter sets fed up front (as the leading stream bytes) are cached and
	/// re-inserted ahead of a later bare IDR, so the keyframe is self-contained
	/// even when the stream never repeats its parameter sets inline.
	#[tokio::test(start_paused = true)]
	async fn params_then_bare_keyframe_self_contained() {
		let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

		let mut split = Split::new();
		// The leading SPS/PPS carry no slice, so they complete no frame yet.
		assert!(split.decode(&annexb(&[sps, pps]), ts()).unwrap().is_empty());

		let frames = decode_one(&mut split, &mut annexb(&[idr]), ts());
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert!(frames[0].payload.windows(sps.len()).any(|w| w == sps));
		assert!(frames[0].payload.windows(pps.len()).any(|w| w == pps));
	}

	/// In streaming mode an access unit completes only once the next one begins
	/// (a slice with first_mb_in_slice set). A keyframe AU followed by a P-slice
	/// of the next AU completes the keyframe; the P-slice's own AU stays buffered
	/// until `flush`.
	#[tokio::test(start_paused = true)]
	async fn decode_emits_on_next_boundary() {
		let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		// P-slice with first_mb_in_slice (byte 1 high bit) set, opening a new AU.
		let pslice: &[u8] = &[0x61, 0xe0, 0x12, 0x34];
		// A trailing AUD so the P-slice is a *complete* NAL (it has a following
		// start code), letting the keyframe boundary be detected during decode.
		let aud: &[u8] = &[0x09, 0x10];

		let mut split = Split::new();
		let frames = split.decode(&annexb(&[sps, pps, idr, pslice, aud]), ts()).unwrap();
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);

		// Flushing closes the buffered P-slice AU (the AUD rides along with it).
		let tail = split.flush(ts()).unwrap();
		assert_eq!(tail.len(), 1);
		assert!(!tail[0].keyframe);
	}
}
