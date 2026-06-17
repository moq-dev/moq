//! H.264 stream splitter.
//!
//! [`Split`] turns raw H.264 bytes into [`crate::container::Frame`]s. It handles
//! both wire shapes:
//!
//! - **avc3** (Annex-B, inline SPS/PPS): finds access-unit boundaries, caches
//!   SPS/PPS and re-inserts them ahead of each keyframe so every keyframe is
//!   self-contained.
//! - **avc1** (length-prefixed NALU, out-of-band avcC): one length-prefixed
//!   access unit per [`decode_frame`](Self::decode_frame) call, with the keyframe
//!   flag set by scanning for an IDR slice.
//!
//! It is deliberately dumb: framing and structural parsing only, plus wall-clock
//! timestamps when the caller has none (stdin). It owns no track, catalog, or
//! codec config (no [`VideoConfig`](hang::catalog::VideoConfig)). The importer
//! parses the codec config out of the frames it emits.
//!
//! The shape is auto-detected from the first bytes ([`decode_frame`](Self::decode_frame)),
//! or pinned ahead of time with [`with_mode`](Self::with_mode). avc1 needs an
//! [`initialize`](Self::initialize) with the avcC to learn the NALU length size;
//! avc3 optionally [seeds](Self::seed) its parameter-set cache.

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Error, NAL_TYPE_PPS, NAL_TYPE_SPS};
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};

/// The wire shape a [`Split`] processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
	/// Length-prefixed NALU with out-of-band AVCDecoderConfigurationRecord
	/// (catalog `H264 { inline: false }`, `description = avcC`).
	Avc1,
	/// Annex-B (start-code prefixed) with inline SPS/PPS
	/// (catalog `H264 { inline: true }`, no description).
	Avc3,
}

/// H.264 stream splitter: bytes in, [`Frame`](crate::container::Frame)s out.
///
/// Handles both wire shapes (avc1 and avc3); the shape is detected from the
/// first bytes [`decode_frame`](Self::decode_frame) sees, or pinned via
/// [`with_mode`](Self::with_mode).
///
/// Feed bytes via [`decode_stream`](Self::decode_stream) (avc3 only, unknown
/// frame boundaries, e.g. stdin), [`decode_frame`](Self::decode_frame) (one
/// complete access unit per call, either shape), or [`decode_from`](Self::decode_from)
/// (an async reader, avc3). Each returns the frames it produced. For avc3,
/// SPS/PPS seen inline are cached and re-inserted ahead of each keyframe;
/// [`seed`](Self::seed) primes that cache from an out-of-band parameter-set
/// buffer. For avc1, [`initialize`](Self::initialize) reads the avcC for the
/// NALU length size.
pub struct Split {
	shape: Shape,
	current: Avc3Frame,
	sps: Option<Bytes>,
	pps: Option<Bytes>,
	zero: Option<tokio::time::Instant>,
	pending: Vec<crate::container::Frame>,
}

/// Internal wire-shape state. Distinct from the public [`Mode`] because avc1
/// carries the resolved NALU length size, and the shape may still be pending
/// auto-detection.
enum Shape {
	/// No bytes seen yet; mode pinned ahead of time or still unknown.
	Pending { hint: Option<Mode> },
	/// avc1: length-prefixed NALU. The NALU length size comes from the avcC.
	Avc1 { length_size: usize },
	/// avc3: Annex-B NALU, inline SPS/PPS.
	Avc3,
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
	/// A fresh splitter with an empty parameter-set cache, wire shape unpinned.
	pub fn new() -> Self {
		Self {
			shape: Shape::Pending { hint: None },
			current: Avc3Frame::default(),
			sps: None,
			pps: None,
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Pin the wire shape ahead of time; skips the leading-bytes auto-detect.
	///
	/// avc1 still needs an [`initialize`](Self::initialize) with the avcC to
	/// learn the NALU length size.
	pub fn with_mode(mut self, mode: Mode) -> Self {
		self.shape = match mode {
			Mode::Avc1 => Shape::Pending { hint: Some(Mode::Avc1) },
			Mode::Avc3 => Shape::Avc3,
		};
		self
	}

	/// Initialize from the codec's leading bytes.
	///
	/// - **avc1**: the buffer is an `AVCDecoderConfigurationRecord`; only the
	///   NALU length size is read out (the importer resolves the codec config).
	///   Required for avc1 before [`decode_frame`](Self::decode_frame).
	/// - **avc3**: the buffer is Annex-B; any SPS/PPS primes the parameter-set
	///   cache so the first keyframe is self-contained (the same as [`seed`](Self::seed)).
	///
	/// The buffer is fully consumed.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let mode = match &self.shape {
			Shape::Pending { hint } => hint.unwrap_or_else(|| detect_mode(buf.as_ref())),
			Shape::Avc1 { .. } => Mode::Avc1,
			Shape::Avc3 => Mode::Avc3,
		};

		match mode {
			Mode::Avc1 => {
				let avcc = super::Avcc::parse(buf.as_ref())?;
				self.shape = Shape::Avc1 {
					length_size: avcc.length_size,
				};
				buf.advance(buf.remaining());
				Ok(())
			}
			Mode::Avc3 => {
				self.shape = Shape::Avc3;
				self.seed(buf)
			}
		}
	}

	/// Prime the SPS/PPS cache from an Annex-B parameter-set buffer, so the first
	/// keyframe is self-contained even if the stream itself omits inline
	/// parameter sets. Other NAL types in the buffer are ignored. avc3 only;
	/// implies the avc3 shape.
	pub fn seed<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		self.shape = Shape::Avc3;
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

	/// Decode from an asynchronous reader, returning all frames produced (avc3).
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<Vec<crate::container::Frame>> {
		let mut frames = Vec::new();
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			frames.extend(self.decode_stream(&mut buffer, None)?);
		}
		Ok(frames)
	}

	/// Decode a buffer where frame boundaries are unknown, returning the frames
	/// it produced. avc3 only (avc1 has no self-delimiting framing). The leading
	/// start code of the *next* access unit is what signals the previous one is
	/// complete, so the final access unit stays buffered until the next call (or
	/// [`decode_frame`](Self::decode_frame)).
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: impl Into<Option<moq_net::Timestamp>>,
	) -> Result<Vec<crate::container::Frame>> {
		match self.shape {
			Shape::Avc3 => {}
			Shape::Pending {
				hint: None | Some(Mode::Avc3),
			} => self.shape = Shape::Avc3,
			Shape::Avc1 { .. } | Shape::Pending { hint: Some(Mode::Avc1) } => {
				return Err(Error::StreamNotAvc3.into());
			}
		}
		let pts = self.pts(pts.into())?;
		let nals = NalIterator::new(buf);
		for nal in nals {
			self.decode_nal(nal?, pts)?;
		}
		Ok(std::mem::take(&mut self.pending))
	}

	/// Decode a buffer holding one complete access unit, returning the frames it
	/// produced (typically one).
	///
	/// - avc3: any trailing NAL without a start code is the last NAL of this
	///   access unit, and the unit is flushed before returning.
	/// - avc1: the buffer is one length-prefixed access unit, emitted as a single
	///   frame with the keyframe flag set if it carries an IDR slice. A missing
	///   `pts` is an error (avc1 always carries timestamps).
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: impl Into<Option<moq_net::Timestamp>>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = pts.into();
		match self.shape {
			Shape::Avc1 { length_size } => {
				let frame = read_avc1_frame(buf, length_size, pts)?;
				Ok(vec![frame])
			}
			Shape::Avc3 => self.decode_frame_avc3(buf, pts),
			Shape::Pending { hint } => match hint.unwrap_or_else(|| detect_mode(buf.as_ref())) {
				Mode::Avc3 => {
					self.shape = Shape::Avc3;
					self.decode_frame_avc3(buf, pts)
				}
				// avc1 needs the avcC (length size) from initialize() first.
				Mode::Avc1 => Err(Error::NotInitialized.into()),
			},
		}
	}

	fn decode_frame_avc3<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<moq_net::Timestamp>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts)?;
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

/// Detect the wire shape from leading bytes: a 3- or 4-byte Annex-B start code
/// means avc3, otherwise an AVCDecoderConfigurationRecord (avc1).
fn detect_mode(bytes: &[u8]) -> Mode {
	if matches!(bytes, [0, 0, 1, ..]) || matches!(bytes, [0, 0, 0, 1, ..]) {
		Mode::Avc3
	} else {
		Mode::Avc1
	}
}

/// Build one avc1 frame from a length-prefixed-NALU buffer, scanning for an IDR
/// to set the keyframe flag. avc1 always carries timestamps, so a missing `pts`
/// is an error.
fn read_avc1_frame<T: Buf + AsRef<[u8]>>(
	buf: &mut T,
	length_size: usize,
	pts: Option<moq_net::Timestamp>,
) -> Result<crate::container::Frame> {
	let data = buf.as_ref();
	let pts = pts.ok_or(Error::MissingTimestamp)?;
	let keyframe = avc1_is_keyframe(data, length_size);
	let frame = crate::container::Frame {
		timestamp: pts,
		payload: data.to_vec().into(),
		keyframe,
		duration: None,
	};
	buf.advance(buf.remaining());
	Ok(frame)
}

/// Detect whether an avc1-shaped (length-prefixed) buffer contains an IDR slice.
fn avc1_is_keyframe(data: &[u8], length_size: usize) -> bool {
	let mut offset = 0;
	while offset + length_size <= data.len() {
		let nal_len = match length_size {
			1 => data[offset] as usize,
			2 => u16::from_be_bytes([data[offset], data[offset + 1]]) as usize,
			3 => u32::from_be_bytes([0, data[offset], data[offset + 1], data[offset + 2]]) as usize,
			4 => u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize,
			_ => return false,
		};
		offset += length_size;
		if offset + nal_len > data.len() {
			break;
		}
		if nal_len > 0 && data[offset] & 0x1f == 5 {
			return true; // IDR slice
		}
		offset += nal_len;
	}
	false
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

	#[test]
	fn detect_mode_avc1_avcc_buffer() {
		// AVCDecoderConfigurationRecord starts with configurationVersion = 1;
		// the first byte is 0x01, never a start code.
		let avcc: &[u8] = &[
			0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x06, 0x67, 0x42, 0xc0, 0x1f, 0xde, 0xad,
		];
		assert_eq!(detect_mode(avcc), Mode::Avc1);
	}

	#[test]
	fn detect_mode_avc3_3byte_start_code() {
		assert_eq!(detect_mode(&[0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1f]), Mode::Avc3);
	}

	#[test]
	fn detect_mode_avc3_4byte_start_code() {
		assert_eq!(
			detect_mode(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1f]),
			Mode::Avc3
		);
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

	/// avc1: a length-prefixed access unit with an IDR slice is emitted as one
	/// keyframe; the payload is passed through verbatim.
	#[tokio::test(start_paused = true)]
	async fn avc1_decode_frame_keyframe() {
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let mut au = BytesMut::new();
		au.extend_from_slice(&(idr.len() as u32).to_be_bytes());
		au.extend_from_slice(idr);

		// avcC with lengthSizeMinusOne = 3 (4-byte length prefix).
		let avcc: &[u8] = &[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f];
		let mut split = Split::new().with_mode(Mode::Avc1);
		split.initialize(&mut avcc.to_vec().as_slice()).unwrap();

		let frames = split
			.decode_frame(&mut au, moq_net::Timestamp::from_micros(0).unwrap())
			.unwrap();
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert_eq!(frames[0].payload[4..], *idr);
	}

	/// avc1: a length-prefixed access unit with a non-IDR slice is a delta frame.
	#[tokio::test(start_paused = true)]
	async fn avc1_decode_frame_delta() {
		let pslice: &[u8] = &[0x61, 0xe0, 0x12, 0x34];
		let mut au = BytesMut::new();
		au.extend_from_slice(&(pslice.len() as u32).to_be_bytes());
		au.extend_from_slice(pslice);

		let avcc: &[u8] = &[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f];
		let mut split = Split::new().with_mode(Mode::Avc1);
		split.initialize(&mut avcc.to_vec().as_slice()).unwrap();

		let frames = split
			.decode_frame(&mut au, moq_net::Timestamp::from_micros(0).unwrap())
			.unwrap();
		assert_eq!(frames.len(), 1);
		assert!(!frames[0].keyframe);
	}

	/// avc1 with no avcC initialize() yet can't know the length size, so a
	/// decode is an error rather than a misparse.
	#[tokio::test(start_paused = true)]
	async fn avc1_decode_before_init_errors() {
		let mut au = BytesMut::from(&[0x00, 0x00, 0x00, 0x04, 0x65, 0x88, 0x84, 0x21][..]);
		let mut split = Split::new().with_mode(Mode::Avc1);
		let err = split
			.decode_frame(&mut au, moq_net::Timestamp::from_micros(0).unwrap())
			.expect_err("avc1 needs initialize() first");
		assert!(matches!(err, crate::Error::H264(Error::NotInitialized)), "got {err:?}");
	}

	/// decode_stream rejects avc1 (no self-delimiting framing).
	#[tokio::test(start_paused = true)]
	async fn avc1_decode_stream_errors() {
		let mut split = Split::new().with_mode(Mode::Avc1);
		let mut buf = BytesMut::from(&[0x00, 0x00, 0x00, 0x04, 0x65, 0x88, 0x84, 0x21][..]);
		let err = split
			.decode_stream(&mut buf, moq_net::Timestamp::from_micros(0).unwrap())
			.expect_err("decode_stream is avc3 only");
		assert!(matches!(err, crate::Error::H264(Error::StreamNotAvc3)), "got {err:?}");
	}
}
