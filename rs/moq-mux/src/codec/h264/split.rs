//! H.264 byte parser for both wire shapes.
//!
//! [`Split`] turns H.264 bytes into [`crate::container::Frame`]s plus a resolved
//! [`hang::catalog::VideoConfig`]. It accepts either length-prefixed NALU input
//! with an out-of-band [`AVCDecoderConfigurationRecord`](super::Avcc) (the "avc1"
//! shape) or Annex-B input with inline SPS/PPS (the "avc3" shape). The shape is
//! detected at [`initialize`](Split::initialize) time by looking for a leading
//! start code; callers that already know it can also force the mode via
//! [`with_mode`](Split::with_mode).
//!
//! Unlike a full importer, [`Split`] owns no track or catalog: it only parses,
//! emitting frames and surfacing the codec config via [`take_config`](Split::take_config).

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Error, Sps};
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};

/// The wire shape a [`Split`] is processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
	/// Length-prefixed NALU with out-of-band AVCDecoderConfigurationRecord
	/// (catalog `H264 { inline: false }`, `description = avcC`).
	Avc1,
	/// Annex-B (start-code prefixed) with inline SPS/PPS
	/// (catalog `H264 { inline: true }`, no description).
	Avc3,
}

/// H.264 byte parser. Handles both avc1 (length-prefixed) and avc3 (Annex-B)
/// input streams; the shape is detected from the first bytes the caller
/// supplies, or forced explicitly via [`with_mode`](Self::with_mode).
///
/// Feed bytes via [`initialize`](Self::initialize), [`decode_frame`](Self::decode_frame),
/// [`decode_stream`](Self::decode_stream), or [`decode_from`](Self::decode_from); each
/// returns the [`Frame`](crate::container::Frame)s it produced. The resolved
/// [`hang::catalog::VideoConfig`] is exposed lazily once the codec config is known
/// (avcC for avc1, the first SPS for avc3) via [`take_config`](Self::take_config).
pub struct Split {
	config: Option<hang::catalog::VideoConfig>,
	config_dirty: bool,
	state: State,
	zero: Option<tokio::time::Instant>,
	pending: Vec<crate::container::Frame>,
}

enum State {
	/// No bytes seen yet; mode pinned ahead of time or unknown.
	Pending { mode_hint: Option<Mode> },
	/// avc1 wire shape: length-prefixed NALU, codec config out-of-band.
	Avc1 { length_size: usize },
	/// avc3 wire shape: Annex-B NALU, inline SPS/PPS.
	Avc3 {
		current: Avc3Frame,
		sps: Option<Bytes>,
		pps: Option<Bytes>,
	},
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
	/// Auto-detect the wire shape from the first bytes supplied to
	/// [`initialize`](Self::initialize).
	pub fn new() -> Self {
		Self {
			config: None,
			config_dirty: false,
			state: State::Pending { mode_hint: None },
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Pin the wire shape ahead of time; skips the leading-bytes auto-detect
	/// inside [`initialize`](Self::initialize).
	pub fn with_mode(mode: Mode) -> Result<Self> {
		let state = match mode {
			Mode::Avc1 => State::Pending {
				mode_hint: Some(Mode::Avc1),
			},
			Mode::Avc3 => State::Avc3 {
				current: Avc3Frame::default(),
				sps: None,
				pps: None,
			},
		};
		Ok(Self {
			config: None,
			config_dirty: false,
			state,
			zero: None,
			pending: Vec::new(),
		})
	}

	/// Initialize from the codec's leading bytes.
	///
	/// - **avc1** (no leading start code): the buffer is parsed as an
	///   `AVCDecoderConfigurationRecord` and stored as the config `description`.
	/// - **avc3** (leading `0x00 0x00 0x01` or `0x00 0x00 0x00 0x01`): the buffer
	///   is parsed as Annex-B NALs to seed the cached SPS/PPS.
	///
	/// The buffer is fully consumed.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let mode = match &self.state {
			State::Pending { mode_hint } => mode_hint.unwrap_or_else(|| detect_mode(buf.as_ref())),
			State::Avc1 { .. } => Mode::Avc1,
			State::Avc3 { .. } => Mode::Avc3,
		};

		match mode {
			Mode::Avc1 => self.initialize_avc1(buf),
			Mode::Avc3 => self.initialize_avc3(buf),
		}
	}

	/// Initialize the avc1 path from an `AVCDecoderConfigurationRecord` buffer.
	fn initialize_avc1<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let avcc_bytes = buf.as_ref();
		let avcc = super::Avcc::parse(avcc_bytes)?;
		self.state = State::Avc1 {
			length_size: avcc.length_size,
		};

		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			profile: avcc.profile,
			constraints: avcc.constraints,
			level: avcc.level,
			inline: false,
		});
		config.coded_width = avcc.coded_width;
		config.coded_height = avcc.coded_height;
		config.description = Some(Bytes::copy_from_slice(avcc_bytes));
		config.container = hang::catalog::Container::Legacy;

		self.swap_config(config)?;
		buf.advance(buf.remaining());

		Ok(())
	}

	/// Initialize the avc3 path by parsing Annex-B NALs (SPS/PPS seed the
	/// config once the first SPS is parsed).
	fn initialize_avc3<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		if !matches!(self.state, State::Avc3 { .. }) {
			self.state = State::Avc3 {
				current: Avc3Frame::default(),
				sps: None,
				pps: None,
			};
		}

		let mut nals = NalIterator::new(buf);
		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, None)?;
		}
		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, None)?;
		}

		Ok(())
	}

	/// True once the codec config has been resolved.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	/// The resolved config if it changed since the last call, else `None`.
	///
	/// Set whenever a new SPS/avcC is parsed; returns `Some` once per change.
	pub fn take_config(&mut self) -> Option<hang::catalog::VideoConfig> {
		if self.config_dirty {
			self.config_dirty = false;
			self.config.clone()
		} else {
			None
		}
	}

	/// Decode from an asynchronous reader, returning all frames produced.
	///
	/// avc3 only. For avc1, the caller already has framed buffers and uses
	/// [`decode_frame`](Self::decode_frame).
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<Vec<crate::container::Frame>> {
		let mut frames = Vec::new();
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			frames.extend(self.decode_stream(&mut buffer, None)?);
		}
		Ok(frames)
	}

	/// Decode a buffer where frame boundaries are unknown (avc3 streaming
	/// input), returning the frames it produced. The leading start code of the
	/// *next* frame is what signals the previous frame is done.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<moq_net::Timestamp>,
	) -> Result<Vec<crate::container::Frame>> {
		if !matches!(self.state, State::Avc3 { .. }) {
			return Err(Error::StreamNotAvc3.into());
		}
		let pts = self.pts(pts)?;
		let nals = NalIterator::new(buf);
		for nal in nals {
			self.decode_nal(nal?, Some(pts))?;
		}
		Ok(std::mem::take(&mut self.pending))
	}

	/// Decode a buffer assumed to hold (the rest of) a single frame, returning
	/// the frames it produced.
	///
	/// - avc1: the buffer is written as one length-prefixed-NALU frame.
	/// - avc3: NALs are parsed; any trailing NAL without a start code is
	///   flushed as the last NAL of this frame.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<moq_net::Timestamp>,
	) -> Result<Vec<crate::container::Frame>> {
		match &self.state {
			State::Avc1 { .. } => self.decode_avc1(buf, pts)?,
			State::Avc3 { .. } => self.decode_avc3_frame(buf, pts)?,
			State::Pending { .. } => return Err(Error::NotInitialized.into()),
		}
		Ok(std::mem::take(&mut self.pending))
	}

	fn decode_avc1<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let State::Avc1 { length_size } = self.state else {
			unreachable!("checked by decode_frame")
		};
		let data = buf.as_ref();
		let pts = self.pts(pts)?;
		let keyframe = avc1_is_keyframe(data, length_size);

		self.pending.push(crate::container::Frame {
			timestamp: pts,
			payload: data.to_vec().into(),
			keyframe,
			duration: None,
		});

		buf.advance(buf.remaining());
		Ok(())
	}

	fn decode_avc3_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let pts = self.pts(pts)?;
		let mut nals = NalIterator::new(buf);
		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, Some(pts))?;
		}
		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, Some(pts))?;
		}
		self.maybe_start_frame(Some(pts))?;
		Ok(())
	}

	fn decode_nal(&mut self, nal: Bytes, pts: Option<moq_net::Timestamp>) -> Result<()> {
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
				let sps = Sps::parse(&nal)?;
				self.init_from_sps(&sps)?;
				let State::Avc3 { current, sps, pps } = &mut self.state else {
					unreachable!("decode_nal is avc3 only")
				};
				if sps.as_ref().is_some_and(|cached| cached != &nal) {
					// SPS changed mid-AU. The cached PPS is tied to the old SPS
					// and may already have been appended to current.chunks
					// earlier in this AU; reset the AU so the new SPS+PPS pair
					// is the only parameter set we emit.
					*pps = None;
					current.chunks.clear();
					current.contains_pps = false;
					current.contains_sps = false;
				}
				*sps = Some(nal.clone());
				current.contains_sps = true;
			}
			Some(Avc3NalType::Pps) => {
				self.maybe_start_frame(pts)?;
				let State::Avc3 { current, pps, .. } = &mut self.state else {
					unreachable!()
				};
				*pps = Some(nal.clone());
				current.contains_pps = true;
			}
			Some(Avc3NalType::Aud) | Some(Avc3NalType::Sei) => {
				self.maybe_start_frame(pts)?;
			}
			Some(Avc3NalType::IdrSlice) => {
				let State::Avc3 { current, sps, pps } = &mut self.state else {
					unreachable!()
				};
				if !current.contains_sps
					&& let Some(sps) = sps.as_ref()
				{
					current.chunks.extend_from_slice(&START_CODE);
					current.chunks.extend_from_slice(sps);
					current.contains_sps = true;
				}
				if !current.contains_pps
					&& let Some(pps) = pps.as_ref()
				{
					current.chunks.extend_from_slice(&START_CODE);
					current.chunks.extend_from_slice(pps);
					current.contains_pps = true;
				}
				current.contains_idr = true;
				current.contains_slice = true;
			}
			Some(Avc3NalType::NonIdrSlice)
			| Some(Avc3NalType::DataPartitionA)
			| Some(Avc3NalType::DataPartitionB)
			| Some(Avc3NalType::DataPartitionC) => {
				if nal.get(1).ok_or(Error::NalTooShort)? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				let State::Avc3 { current, .. } = &mut self.state else {
					unreachable!()
				};
				current.contains_slice = true;
			}
			_ => {}
		}

		tracing::trace!(kind = ?nal_type, "parsed NAL");

		let State::Avc3 { current, .. } = &mut self.state else {
			unreachable!()
		};
		current.chunks.extend_from_slice(&START_CODE);
		current.chunks.extend_from_slice(&nal);
		Ok(())
	}

	fn init_from_sps(&mut self, sps: &Sps) -> Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			profile: sps.profile,
			constraints: sps.constraints,
			level: sps.level,
			inline: true,
		});
		config.coded_width = Some(sps.coded_width);
		config.coded_height = Some(sps.coded_height);
		config.container = hang::catalog::Container::Legacy;

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		// avc3 carries SPS inline, so a resolution change updates the config in
		// place (no new init segment, unlike avc1).
		self.config = Some(config);
		self.config_dirty = true;
		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let State::Avc3 { current, .. } = &mut self.state else {
			return Ok(());
		};
		if !current.contains_slice {
			return Ok(());
		}
		let pts = pts.ok_or(Error::MissingTimestamp)?;
		let payload = std::mem::take(&mut current.chunks).freeze();
		let keyframe = current.contains_idr;
		current.contains_idr = false;
		current.contains_slice = false;
		current.contains_sps = false;
		current.contains_pps = false;

		self.pending.push(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe,
			duration: None,
		});
		Ok(())
	}

	/// Resolve the avc1 codec config.
	///
	/// The first config is stored. A different avcC would mean a new init
	/// segment, which a single fixed track can't represent, so a reconfiguration
	/// is an error (mint a new track via a fresh parser).
	fn swap_config(&mut self, config: hang::catalog::VideoConfig) -> Result<()> {
		if let Some(old) = &self.config {
			if old == &config {
				return Ok(());
			}
			return Err(Error::FixedTrackReconfigured.into());
		}

		tracing::debug!(?config, "starting H.264 track");
		self.config = Some(config);
		self.config_dirty = true;
		Ok(())
	}

	/// Drop any in-flight avc3 access unit.
	///
	/// Pre-reset NALs would otherwise leak into a later frame with the wrong
	/// timestamp.
	pub fn reset(&mut self) {
		if let State::Avc3 { current, .. } = &mut self.state {
			*current = Avc3Frame::default();
		}
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}
		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

/// Detect the wire shape from leading bytes: a 3- or 4-byte Annex-B start
/// code means avc3, otherwise an AVCDecoderConfigurationRecord (avc1).
fn detect_mode(bytes: &[u8]) -> Mode {
	let three_byte = matches!(bytes, [0, 0, 1, ..]);
	let four_byte = matches!(bytes, [0, 0, 0, 1, ..]);
	if three_byte || four_byte {
		Mode::Avc3
	} else {
		Mode::Avc1
	}
}

/// Detect if an avc1-shaped (length-prefixed) buffer contains an IDR slice.
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

	#[test]
	fn detect_mode_avc1_avcc_buffer() {
		// AVCDecoderConfigurationRecord starts with configurationVersion = 1, profile, ...
		// First byte is 0x01, definitely not a start code.
		let avcc: &[u8] = &[
			0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x06, 0x67, 0x42, 0xc0, 0x1f, 0xde, 0xad,
		];
		assert_eq!(detect_mode(avcc), Mode::Avc1);
	}

	#[test]
	fn detect_mode_avc3_3byte_start_code() {
		let nals: &[u8] = &[0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1f];
		assert_eq!(detect_mode(nals), Mode::Avc3);
	}

	#[test]
	fn detect_mode_avc3_4byte_start_code() {
		let nals: &[u8] = &[0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1f];
		assert_eq!(detect_mode(nals), Mode::Avc3);
	}

	/// Auto-detect routes an avcC initializer into the avc1 path and resolves a
	/// config with the avcC stored as `description`.
	#[tokio::test(start_paused = true)]
	async fn auto_detect_avc1_lands_in_catalog() {
		// Minimal AVCDecoderConfigurationRecord: version(1) profile(0x42) compat(0xc0) level(0x1f)
		// length_size_minus_one + 0xfc | 3 = 0xff
		// reserved | num_sps = 0xe1
		// sps_len = 4, sps bytes (NAL header 0x67 + profile/level for parsing).
		let sps_nal = [0x67, 0x42, 0xc0, 0x1f];
		let mut avcc = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps_nal.len() as u8];
		avcc.extend_from_slice(&sps_nal);
		avcc.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]); // num_pps + pps

		let mut split = Split::new();
		let mut buf = bytes::BytesMut::from(avcc.as_slice());
		split.initialize(&mut buf).expect("initialize avc1");

		let cfg = split.take_config().expect("config known after init");
		let hang::catalog::VideoCodec::H264(h264) = &cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(!h264.inline, "avc1 source should land as inline=false");
		assert_eq!(h264.profile, 0x42);
		assert_eq!(h264.level, 0x1f);
		let desc = cfg.description.as_ref().expect("description set");
		assert_eq!(desc.as_ref(), avcc.as_slice());
	}

	/// Auto-detect routes an Annex-B initializer into the avc3 path; the
	/// resolved config reports inline=true and no description.
	#[tokio::test(start_paused = true)]
	async fn auto_detect_avc3_lands_in_catalog() {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let mut annexb = bytes::BytesMut::new();
		annexb.extend_from_slice(&[0, 0, 0, 1]);
		annexb.extend_from_slice(sps);
		annexb.extend_from_slice(&[0, 0, 0, 1]);
		annexb.extend_from_slice(pps);

		let mut split = Split::new();
		split.initialize(&mut annexb).expect("initialize avc3");

		let cfg = split.take_config().expect("config known after first SPS");
		let hang::catalog::VideoCodec::H264(h264) = &cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(h264.inline, "avc3 source should land as inline=true");
		assert!(cfg.description.is_none(), "avc3 has no out-of-band description");
		assert_eq!(h264.profile, sps[1]);
		assert_eq!(h264.level, sps[3]);
	}
}
