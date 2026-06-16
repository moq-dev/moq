//! H.264 Annex-B stream splitter.
//!
//! [`Split`] turns a raw Annex-B byte stream (inline SPS/PPS, the "avc3" shape)
//! into [`crate::container::Frame`]s. It is deliberately dumb: it finds
//! access-unit boundaries, caches SPS/PPS and re-inserts them ahead of each
//! keyframe so every keyframe is self-contained, and stamps wall-clock
//! timestamps when the caller has none (stdin). It owns no track, catalog, or
//! codec config. The importer parses the codec config out of the frames it
//! emits.
//!
//! There is no out-of-band initialization beyond optionally [seeding](Self::seed)
//! the parameter-set cache: a caller that can configure a decoder out of band
//! already knows frame boundaries, and would hand whole frames to the importer
//! rather than a byte stream.

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Error, NAL_TYPE_PPS, NAL_TYPE_SPS};
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};

/// H.264 Annex-B stream splitter: bytes in, [`Frame`](crate::container::Frame)s out.
///
/// Feed bytes via [`decode_stream`](Self::decode_stream) (unknown frame
/// boundaries, e.g. stdin), [`decode_frame`](Self::decode_frame) (one complete
/// access unit per call), or [`decode_from`](Self::decode_from) (an async
/// reader). Each returns the frames it produced. SPS/PPS seen inline are cached
/// and re-inserted ahead of each keyframe; [`seed`](Self::seed) primes that
/// cache from an out-of-band parameter-set buffer.
pub struct Split {
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
			current: Avc3Frame::default(),
			sps: None,
			pps: None,
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Prime the SPS/PPS cache from an Annex-B parameter-set buffer, so the first
	/// keyframe is self-contained even if the stream itself omits inline
	/// parameter sets. Other NAL types in the buffer are ignored.
	pub fn seed<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let mut nals = NalIterator::new(buf);
		while let Some(nal) = nals.next().transpose()? {
			self.cache_param(&nal);
		}
		if let Some(nal) = nals.flush()? {
			self.cache_param(&nal);
		}
		Ok(())
	}

	fn cache_param(&mut self, nal: &Bytes) {
		match nal.first().map(|h| h & 0x1f) {
			Some(NAL_TYPE_SPS) => self.sps = Some(nal.clone()),
			Some(NAL_TYPE_PPS) => self.pps = Some(nal.clone()),
			_ => {}
		}
	}

	/// Decode from an asynchronous reader, returning all frames produced.
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<Vec<crate::container::Frame>> {
		let mut frames = Vec::new();
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			frames.extend(self.decode_stream(&mut buffer, None)?);
		}
		Ok(frames)
	}

	/// Decode a buffer where frame boundaries are unknown, returning the frames
	/// it produced. The leading start code of the *next* access unit is what
	/// signals the previous one is complete, so the final access unit stays
	/// buffered until the next call (or [`decode_frame`](Self::decode_frame)).
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: impl Into<Option<moq_net::Timestamp>>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts.into())?;
		let nals = NalIterator::new(buf);
		for nal in nals {
			self.decode_nal(nal?, pts)?;
		}
		Ok(std::mem::take(&mut self.pending))
	}

	/// Decode a buffer holding one complete access unit, returning the frames it
	/// produced (typically one). Any trailing NAL without a start code is the
	/// last NAL of this access unit, and the unit is flushed before returning.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: impl Into<Option<moq_net::Timestamp>>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts.into())?;
		let mut nals = NalIterator::new(buf);
		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, pts)?;
		}
		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, pts)?;
		}
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

	/// A keyframe access unit fed as one buffer emits one self-contained frame:
	/// SPS+PPS are packaged ahead of the IDR slice and `keyframe` is set.
	#[tokio::test(start_paused = true)]
	async fn decode_frame_packages_keyframe() {
		let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

		let mut split = Split::new();
		let mut buf = annexb(&[sps, pps, idr]);
		let frames = split
			.decode_frame(&mut buf, moq_net::Timestamp::from_micros(0).unwrap())
			.unwrap();

		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		// The payload carries SPS, PPS, then the IDR slice (each start-code prefixed).
		assert_eq!(&frames[0].payload[..SC4.len()], SC4);
		assert!(frames[0].payload.windows(sps.len()).any(|w| w == sps));
		assert!(frames[0].payload.windows(idr.len()).any(|w| w == idr));
	}

	/// A seeded splitter re-inserts the cached SPS/PPS ahead of a bare IDR slice,
	/// even though the stream itself never carried inline parameter sets.
	#[tokio::test(start_paused = true)]
	async fn seed_makes_bare_keyframe_self_contained() {
		let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

		let mut split = Split::new();
		split.seed(&mut annexb(&[sps, pps])).unwrap();

		let frames = split
			.decode_frame(&mut annexb(&[idr]), moq_net::Timestamp::from_micros(0).unwrap())
			.unwrap();
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert!(frames[0].payload.windows(sps.len()).any(|w| w == sps));
		assert!(frames[0].payload.windows(pps.len()).any(|w| w == pps));
	}
}
