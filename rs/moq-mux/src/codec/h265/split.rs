//! H.265 Annex-B stream splitter.
//!
//! The H.265 analogue of [`crate::codec::h264::Split`]: turns a raw Annex-B byte
//! stream (inline VPS/SPS/PPS) into [`crate::container::Frame`]s. It finds
//! access-unit boundaries, caches VPS/SPS/PPS and re-inserts them ahead of each
//! keyframe so every keyframe is self-contained, and stamps wall-clock
//! timestamps when the caller has none (stdin). It owns no track, catalog, or
//! codec config. The importer parses the codec config out of the frames it
//! emits.

use bytes::{Buf, Bytes, BytesMut};
use scuffle_h265::NALUnitType;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::Error;
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};

/// H.265 Annex-B stream splitter: bytes in, [`Frame`](crate::container::Frame)s out.
///
/// Feed bytes via [`decode_stream`](Self::decode_stream) (unknown frame
/// boundaries, e.g. stdin), [`decode_frame`](Self::decode_frame) (one complete
/// access unit per call), or [`decode_from`](Self::decode_from) (an async
/// reader). Each returns the frames it produced. VPS/SPS/PPS seen inline are
/// cached and re-inserted ahead of each keyframe; [`seed`](Self::seed) primes
/// that cache from an out-of-band parameter-set buffer.
pub struct Split {
	current: Au,
	vps: Option<Bytes>,
	sps: Option<Bytes>,
	pps: Option<Bytes>,
	zero: Option<tokio::time::Instant>,
	pending: Vec<crate::container::Frame>,
}

#[derive(Default)]
struct Au {
	chunks: BytesMut,
	contains_idr: bool,
	contains_slice: bool,
	contains_vps: bool,
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
			current: Au::default(),
			vps: None,
			sps: None,
			pps: None,
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Prime the VPS/SPS/PPS cache from an Annex-B parameter-set buffer, so the
	/// first keyframe is self-contained even if the stream itself omits inline
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
		match nal.first().map(|h| nal_unit_type(*h)) {
			Some(NALUnitType::VpsNut) => self.vps = Some(nal.clone()),
			Some(NALUnitType::SpsNut) => self.sps = Some(nal.clone()),
			Some(NALUnitType::PpsNut) => self.pps = Some(nal.clone()),
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

	/// Decode a single NAL unit. Only reads the first header byte to extract
	/// nal_unit_type, ignoring nuh_layer_id and nuh_temporal_id_plus1.
	fn decode_nal(&mut self, nal: Bytes, pts: moq_net::Timestamp) -> Result<()> {
		if nal.len() < 2 {
			return Err(Error::NalTooShort.into());
		}
		// u16 header: [forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)]
		let header = nal.first().ok_or(Error::NalTooShort)?;
		if (header >> 7) & 1 != 0 {
			return Err(Error::ForbiddenZeroBit.into());
		}

		let nal_type = nal_unit_type(*header);

		match nal_type {
			NALUnitType::VpsNut => {
				self.maybe_start_frame(pts)?;
				self.vps = Some(nal.clone());
				self.current.contains_vps = true;
			}
			NALUnitType::SpsNut => {
				self.maybe_start_frame(pts)?;

				// SPS changed mid-stream. Cached PPS is tied to the old SPS and
				// may already have been appended to current.chunks earlier in
				// this AU; reset so the new VPS+SPS+PPS triple is the only
				// parameter set we emit.
				if self.sps.as_ref().is_some_and(|cached| cached != &nal) {
					self.pps = None;
					self.current.chunks.clear();
					self.current.contains_vps = false;
					self.current.contains_sps = false;
					self.current.contains_pps = false;
				}

				self.sps = Some(nal.clone());
				self.current.contains_sps = true;
			}
			NALUnitType::PpsNut => {
				self.maybe_start_frame(pts)?;
				self.pps = Some(nal.clone());
				self.current.contains_pps = true;
			}
			NALUnitType::AudNut | NALUnitType::PrefixSeiNut | NALUnitType::SuffixSeiNut => {
				self.maybe_start_frame(pts)?;
			}
			// Keyframe containing slices.
			NALUnitType::IdrWRadl
			| NALUnitType::IdrNLp
			| NALUnitType::BlaNLp
			| NALUnitType::BlaWRadl
			| NALUnitType::BlaWLp
			| NALUnitType::CraNut => {
				// Insert cached VPS/SPS/PPS before keyframes if not already present.
				if !self.current.contains_vps
					&& let Some(vps) = self.vps.clone()
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(&vps);
					self.current.contains_vps = true;
				}
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
			// All other slice types (both N and R variants).
			NALUnitType::TrailN
			| NALUnitType::TrailR
			| NALUnitType::TsaN
			| NALUnitType::TsaR
			| NALUnitType::StsaN
			| NALUnitType::StsaR
			| NALUnitType::RadlN
			| NALUnitType::RadlR
			| NALUnitType::RaslN
			| NALUnitType::RaslR => {
				// Check first_slice_segment_in_pic_flag (bit 7 of third byte, after 2-byte header).
				if nal.get(2).ok_or(Error::NalTooShort)? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				self.current.contains_slice = true;
			}
			_ => {}
		}

		// Replace the original start code with a canonical 4-byte start code (marginally
		// easier for downstream players, e.g. MSE).
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
		self.current.contains_vps = false;
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
		self.current = Au::default();
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}
		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

/// Extract the HEVC `nal_unit_type` from the first header byte (bits 1..=6).
pub(super) fn nal_unit_type(header: u8) -> NALUnitType {
	NALUnitType::from((header >> 1) & 0b111111)
}

#[cfg(test)]
mod tests {
	use super::*;

	const SC4: &[u8] = &[0, 0, 0, 1];

	// HEVC NAL headers: byte0 = nal_unit_type << 1 (forbidden bit 0, layer id 0).
	const VPS: &[u8] = &[0x40, 0x01, 0x0c]; // type 32
	const SPS: &[u8] = &[0x42, 0x01, 0x01]; // type 33
	const PPS: &[u8] = &[0x44, 0x01, 0xc0]; // type 34
	const IDR: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // type 19 (IdrWRadl)

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

	fn contains(haystack: &[u8], needle: &[u8]) -> bool {
		haystack.windows(needle.len()).any(|w| w == needle)
	}

	/// A keyframe access unit fed as one buffer emits one self-contained frame:
	/// VPS+SPS+PPS are packaged ahead of the IDR slice and `keyframe` is set.
	#[tokio::test(start_paused = true)]
	async fn decode_frame_packages_keyframe() {
		let mut split = Split::new();
		let frames = split.decode_frame(&mut annexb(&[VPS, SPS, PPS, IDR]), ts()).unwrap();

		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert!(contains(&frames[0].payload, VPS));
		assert!(contains(&frames[0].payload, SPS));
		assert!(contains(&frames[0].payload, PPS));
		assert!(contains(&frames[0].payload, IDR));
	}

	/// A seeded splitter re-inserts the cached VPS/SPS/PPS ahead of a bare IDR,
	/// even though the stream itself never carried inline parameter sets.
	#[tokio::test(start_paused = true)]
	async fn seed_makes_bare_keyframe_self_contained() {
		let mut split = Split::new();
		split.seed(&mut annexb(&[VPS, SPS, PPS])).unwrap();

		let frames = split.decode_frame(&mut annexb(&[IDR]), ts()).unwrap();
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert!(contains(&frames[0].payload, VPS));
		assert!(contains(&frames[0].payload, SPS));
		assert!(contains(&frames[0].payload, PPS));
	}
}
