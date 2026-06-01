//! H.264 importer for both wire shapes.
//!
//! [`Import`] accepts either length-prefixed NALU input with an
//! out-of-band [`AVCDecoderConfigurationRecord`](super::Avcc) (the "avc1"
//! shape) or Annex-B input with inline SPS/PPS (the "avc3" shape). The shape
//! is detected at [`initialize`](Import::initialize) time by looking for a
//! leading start code; callers that already know it can also force the
//! mode via [`with_mode`](Import::with_mode).

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Error, Sps};
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};
use crate::container::jitter::MinFrameDuration;

/// The wire shape an [`Import`] is processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
	/// Length-prefixed NALU with out-of-band AVCDecoderConfigurationRecord
	/// (catalog `H264 { inline: false }`, `description = avcC`).
	Avc1,
	/// Annex-B (start-code prefixed) with inline SPS/PPS
	/// (catalog `H264 { inline: true }`, no description).
	Avc3,
}

/// H.264 importer. Handles both avc1 (length-prefixed) and avc3 (Annex-B)
/// input streams; the shape is detected from the first bytes the caller
/// supplies, or forced explicitly via [`with_mode`](Self::with_mode).
pub struct Import {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::hang::Producer,
	track: Option<crate::container::Producer<crate::catalog::hang::Container>>,
	config: Option<hang::catalog::VideoConfig>,
	state: State,
	zero: Option<tokio::time::Instant>,
	jitter: MinFrameDuration,
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

impl Import {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::hang::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			state: State::Pending { mode_hint: None },
			zero: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Pin the wire shape ahead of time; skips the leading-bytes auto-detect
	/// inside [`initialize`](Self::initialize). Eagerly creates the broadcast
	/// track for avc3 sources so the caller can observe subscriber state
	/// (`used()` / `unused()`) before any frames arrive.
	pub fn with_mode(mut self, mode: Mode) -> Result<Self> {
		match mode {
			Mode::Avc1 => {
				self.state = State::Pending {
					mode_hint: Some(Mode::Avc1),
				};
			}
			Mode::Avc3 => {
				let track = self.broadcast.create_track(
					moq_net::Track::new(self.broadcast.unique_name(".avc3")).with_timescale(hang::container::TIMESCALE),
				)?;
				self.track = Some(crate::container::Producer::new(
					track,
					crate::catalog::hang::Container::Legacy,
				));
				self.state = State::Avc3 {
					current: Avc3Frame::default(),
					sps: None,
					pps: None,
				};
			}
		}
		Ok(self)
	}

	/// Returns a reference to the underlying track producer, e.g. for
	/// monitoring subscriber state via `used()` / `unused()`. Available only
	/// after the track has been created. i.e. after [`with_mode`](Self::with_mode)
	/// for avc3 or after [`initialize`](Self::initialize) for avc1.
	pub fn track(&self) -> Option<&moq_net::TrackProducer> {
		self.track.as_ref().map(|t| t.track())
	}

	/// Initialize from the codec's leading bytes.
	///
	/// - **avc1** (no leading start code): the buffer is parsed as an
	///   `AVCDecoderConfigurationRecord` and stored as the catalog `description`.
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

		self.swap_config(config, ".avc1")?;
		buf.advance(buf.remaining());

		Ok(())
	}

	/// Initialize the avc3 path by parsing Annex-B NALs (SPS/PPS seed the
	/// catalog rendition; the track is created eagerly on first SPS).
	fn initialize_avc3<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		// Eager-create the track + state on first switch into Avc3 mode so
		// callers can observe `used()` / `unused()` before any frames arrive.
		if !matches!(self.state, State::Avc3 { .. }) {
			self.state = State::Avc3 {
				current: Avc3Frame::default(),
				sps: None,
				pps: None,
			};
			if self.track.is_none() {
				let track = self.broadcast.create_track(
					moq_net::Track::new(self.broadcast.unique_name(".avc3")).with_timescale(hang::container::TIMESCALE),
				)?;
				self.track = Some(crate::container::Producer::new(
					track,
					crate::catalog::hang::Container::Legacy,
				));
			}
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

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
	}

	/// Decode from an asynchronous reader. avc3 only — for avc1, the caller
	/// already has framed buffers and uses [`decode_frame`](Self::decode_frame).
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<()> {
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			self.decode_stream(&mut buffer, None)?;
		}
		Ok(())
	}

	/// Decode a buffer where frame boundaries are unknown (avc3 streaming
	/// input). The leading start code of the *next* frame is what signals the
	/// previous frame is done.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		if !matches!(self.state, State::Avc3 { .. }) {
			return Err(Error::StreamNotAvc3.into());
		}
		let pts = self.pts(pts)?;
		let nals = NalIterator::new(buf);
		for nal in nals {
			self.decode_nal(nal?, Some(pts))?;
		}
		Ok(())
	}

	/// Decode a buffer assumed to hold (the rest of) a single frame.
	///
	/// - avc1: the buffer is written as one length-prefixed-NALU frame.
	/// - avc3: NALs are parsed; any trailing NAL without a start code is
	///   flushed as the last NAL of this frame.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		match &self.state {
			State::Avc1 { .. } => self.decode_avc1(buf, pts),
			State::Avc3 { .. } => self.decode_avc3_frame(buf, pts),
			State::Pending { .. } => Err(Error::NotInitialized.into()),
		}
	}

	fn decode_avc1<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		let State::Avc1 { length_size } = self.state else {
			unreachable!("checked by decode_frame")
		};
		let data = buf.as_ref();
		let pts = self.pts(pts)?;
		let keyframe = avc1_is_keyframe(data, length_size);
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;

		track.write(crate::container::Frame {
			timestamp: pts,
			payload: data.to_vec().into(),
			keyframe,
			duration: None,
		})?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&track.name)
		{
			c.jitter = Some(jitter);
		}

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

		// The avc3 track was created eagerly in initialize_avc3; just publish
		// (or republish) the catalog rendition with the latest config.
		let track_name = self.track.as_ref().ok_or(Error::Avc3TrackNotCreated)?.name.clone();
		let mut catalog = self.catalog.lock();
		catalog.video.renditions.insert(track_name, config.clone());
		self.config = Some(config);
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

		let track = self.track.as_mut().ok_or(Error::Avc3TrackNotCreated)?;
		track.write(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe,
			duration: None,
		})?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&track.name)
		{
			c.jitter = Some(jitter);
		}
		Ok(())
	}

	/// Replace the current track + catalog rendition with `config`. Used by
	/// the avc1 path on every (re)initialization.
	fn swap_config(&mut self, config: hang::catalog::VideoConfig, suffix: &str) -> Result<()> {
		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		let mut catalog = self.catalog.lock();
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.name, "reinitializing H.264 track");
			catalog.video.renditions.remove(&track.name);
		}
		let track = self.broadcast.create_track(
			moq_net::Track::new(self.broadcast.unique_name(suffix)).with_timescale(hang::container::TIMESCALE),
		)?;
		tracing::debug!(name = ?track.name, ?config, "starting H.264 track");
		catalog.video.renditions.insert(track.name.clone(), config.clone());

		self.config = Some(config);
		self.track = Some(crate::container::Producer::new(
			track,
			crate::catalog::hang::Container::Legacy,
		));
		Ok(())
	}

	/// Finish the track, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;
		track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	///
	/// Any in-flight avc3 access unit is dropped. Pre-seek NALs would otherwise
	/// leak into the post-seek group with the wrong timestamp.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		if let State::Avc3 { current, .. } = &mut self.state {
			*current = Avc3Frame::default();
		}
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;
		track.seek(sequence)?;
		Ok(())
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}
		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

impl Drop for Import {
	fn drop(&mut self) {
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.name, "ending H.264 track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}
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

	/// Auto-detect routes an avcC initializer into the avc1 path and stores
	/// it in the catalog `description`.
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

		let broadcast = moq_net::Broadcast::new();
		let mut producer = broadcast.produce();
		let catalog = crate::catalog::hang::Producer::new(&mut producer).unwrap();

		let mut importer = Import::new(producer, catalog.clone());
		let mut buf = bytes::BytesMut::from(avcc.as_slice());
		importer.initialize(&mut buf).expect("initialize avc1");

		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.video.renditions.len(), 1);
		let cfg = snapshot.video.renditions.values().next().unwrap();
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
	/// catalog rendition reports inline=true and no description.
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

		let broadcast = moq_net::Broadcast::new();
		let mut producer = broadcast.produce();
		let catalog = crate::catalog::hang::Producer::new(&mut producer).unwrap();

		let mut importer = Import::new(producer, catalog.clone());
		importer.initialize(&mut annexb).expect("initialize avc3");

		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.video.renditions.len(), 1);
		let cfg = snapshot.video.renditions.values().next().unwrap();
		let hang::catalog::VideoCodec::H264(h264) = &cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(h264.inline, "avc3 source should land as inline=true");
		assert!(cfg.description.is_none(), "avc3 has no out-of-band description");
		assert_eq!(h264.profile, sps[1]);
		assert_eq!(h264.level, sps[3]);
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
