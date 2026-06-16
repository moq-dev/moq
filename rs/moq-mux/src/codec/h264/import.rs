//! H.264 importer.
//!
//! [`Import`] publishes H.264 frames on a single moq track and resolves the
//! catalog rendition. It accepts either length-prefixed NALU input with an
//! out-of-band [`AVCDecoderConfigurationRecord`](super::Avcc) (the "avc1" shape)
//! or Annex-B input with inline SPS/PPS (the "avc3" shape). The shape is detected
//! from the first bytes the caller supplies, or forced via
//! [`with_mode`](Import::with_mode).
//!
//! The codec config comes from exactly one of two places: an avcC handed to
//! [`initialize`](Import::initialize) (avc1), or the SPS that the splitter
//! packages into the first keyframe (avc3, scanned out of the frame here). A
//! keyframe that can't be configured from either is an error; non-keyframes
//! before the first config are tolerated (mid-stream joins). Annex-B byte
//! parsing lives in [`Split`]; this type drives it and adds the catalog.

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Error, NAL_TYPE_SPS, Split, Sps};
use crate::Result;
use crate::codec::annexb::NalIterator;
use crate::container::Frame;
use crate::container::jitter::MinFrameDuration;
use crate::publish::{FrameDecode, Renditions};

/// The wire shape an [`Import`] processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
	/// Length-prefixed NALU with out-of-band AVCDecoderConfigurationRecord
	/// (catalog `H264 { inline: false }`, `description = avcC`).
	Avc1,
	/// Annex-B (start-code prefixed) with inline SPS/PPS
	/// (catalog `H264 { inline: true }`, no description).
	Avc3,
}

/// H.264 importer. Handles both avc1 (length-prefixed) and avc3 (Annex-B) input;
/// the shape is detected from the first bytes the caller supplies, or forced via
/// [`with_mode`](Self::with_mode).
///
/// Build it from a [`moq_net::TrackRequest`] ([`new`](Self::new), the on-demand
/// path) or an existing [`moq_net::TrackProducer`] ([`from_track`](Self::from_track),
/// the broadcast-push / fixed-track path). The catalog rendition fills in lazily
/// once the codec config is known (avcC for avc1, the first SPS for avc3); read it
/// via [`catalog`](Self::catalog) or attach the importer to a broadcast catalog
/// with [`crate::publish::Published`].
pub struct Import {
	shape: Shape,
	split: Split,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: hang::Catalog,
	config: Option<hang::catalog::VideoConfig>,
	last_sps: Option<Bytes>,
	jitter: MinFrameDuration,
}

enum Shape {
	/// No bytes seen yet; mode pinned ahead of time or still unknown.
	Pending { hint: Option<Mode> },
	/// avc1: length-prefixed NALU, codec config out-of-band (avcC).
	Avc1 { length_size: usize },
	/// avc3: Annex-B NALU, inline SPS/PPS.
	Avc3,
}

impl Import {
	/// Serve a track request, accepting it at the microsecond timescale.
	pub fn new(request: moq_net::TrackRequest) -> Self {
		let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
		Self::from_track(request.accept(info))
	}

	/// Publish on an existing track producer.
	pub fn from_track(track: moq_net::TrackProducer) -> Self {
		Self {
			shape: Shape::Pending { hint: None },
			split: Split::new(),
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog: hang::Catalog::default(),
			config: None,
			last_sps: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Pin the wire shape ahead of time; skips the leading-bytes auto-detect.
	///
	/// avc1 still needs an [`initialize`](Self::initialize) with the avcC to
	/// learn the NALU length size and codec config.
	pub fn with_mode(mut self, mode: Mode) -> Result<Self> {
		self.shape = match mode {
			Mode::Avc1 => Shape::Pending { hint: Some(Mode::Avc1) },
			Mode::Avc3 => Shape::Avc3,
		};
		Ok(self)
	}

	/// Initialize from the codec's leading bytes.
	///
	/// - **avc1** (no leading start code): the buffer is parsed as an
	///   `AVCDecoderConfigurationRecord`, which resolves the config and is stored
	///   as the catalog `description`. Required for avc1.
	/// - **avc3** (leading start code): the buffer is parsed as Annex-B; any SPS
	///   resolves the config and primes the splitter's parameter-set cache.
	///   Optional, since avc3 also self-initializes from the first keyframe.
	///
	/// The buffer is fully consumed.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let mode = match &self.shape {
			Shape::Pending { hint } => hint.unwrap_or_else(|| detect_mode(buf.as_ref())),
			Shape::Avc1 { .. } => Mode::Avc1,
			Shape::Avc3 => Mode::Avc3,
		};

		match mode {
			Mode::Avc1 => self.initialize_avc1(buf),
			Mode::Avc3 => self.initialize_avc3(buf),
		}
	}

	fn initialize_avc1<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let avcc_bytes = buf.as_ref();
		let avcc = super::Avcc::parse(avcc_bytes)?;
		self.shape = Shape::Avc1 {
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

		self.set_config(config)?;
		buf.advance(buf.remaining());
		Ok(())
	}

	fn initialize_avc3<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		self.shape = Shape::Avc3;

		// Resolve the config from any SPS in the seed buffer, then prime the
		// splitter's cache so the first keyframe is self-contained.
		let mut scan = Bytes::copy_from_slice(buf.as_ref());
		let mut nals = NalIterator::new(&mut scan);
		while let Some(nal) = nals.next().transpose()? {
			if is_sps(&nal) {
				self.configure_from_sps(&nal)?;
			}
		}
		if let Some(nal) = nals.flush()?
			&& is_sps(&nal)
		{
			self.configure_from_sps(&nal)?;
		}

		self.split.seed(buf)?;
		Ok(())
	}

	/// The standalone catalog once the codec config is known, else `None`.
	pub fn catalog(&self) -> Option<&hang::Catalog> {
		self.config.is_some().then_some(&self.catalog)
	}

	/// The underlying track producer.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
	}

	/// True once the codec config is known and the catalog rendition is published.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	/// Decode from an asynchronous reader (avc3 streaming input).
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<()> {
		let mut buffer = BytesMut::new();
		while reader.read_buf(&mut buffer).await? > 0 {
			self.decode_stream(&mut buffer, None)?;
		}
		Ok(())
	}

	/// Decode a buffer holding one complete frame.
	///
	/// - avc1: the buffer is one length-prefixed-NALU access unit.
	/// - avc3: the buffer is one Annex-B access unit.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		match self.shape {
			Shape::Avc1 { length_size } => {
				let frame = read_avc1_frame(buf, length_size, pts)?;
				self.write_frames([frame])
			}
			Shape::Avc3 => {
				let frames = self.split.decode_frame(buf, pts)?;
				self.write_frames(frames)
			}
			Shape::Pending { hint } => match hint.unwrap_or_else(|| detect_mode(buf.as_ref())) {
				Mode::Avc3 => {
					self.shape = Shape::Avc3;
					let frames = self.split.decode_frame(buf, pts)?;
					self.write_frames(frames)
				}
				// avc1 needs the avcC (length size) from initialize() first.
				Mode::Avc1 => Err(Error::NotInitialized.into()),
			},
		}
	}

	/// Decode a buffer where frame boundaries are unknown (avc3 streaming input).
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		match self.shape {
			Shape::Avc3 => {}
			Shape::Pending {
				hint: None | Some(Mode::Avc3),
			} => self.shape = Shape::Avc3,
			Shape::Avc1 { .. } | Shape::Pending { hint: Some(Mode::Avc1) } => {
				return Err(Error::StreamNotAvc3.into());
			}
		}
		let frames = self.split.decode_stream(buf, pts)?;
		self.write_frames(frames)
	}

	/// Finish the track, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	///
	/// Any in-flight avc3 access unit is dropped. Pre-seek NALs would otherwise
	/// leak into the post-seek group with the wrong timestamp.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.split.reset();
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Resolve the avc3 config from an inline SPS, updating it in place.
	///
	/// avc3 carries SPS inline, so a resolution change just updates the config
	/// (no new init segment, unlike avc1).
	fn configure_from_sps(&mut self, sps_nal: &Bytes) -> Result<()> {
		if self.last_sps.as_ref() == Some(sps_nal) {
			return Ok(());
		}
		let sps = Sps::parse(sps_nal)?;
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			profile: sps.profile,
			constraints: sps.constraints,
			level: sps.level,
			inline: true,
		});
		config.coded_width = Some(sps.coded_width);
		config.coded_height = Some(sps.coded_height);
		config.container = hang::catalog::Container::Legacy;

		self.last_sps = Some(sps_nal.clone());
		self.apply_config(config);
		Ok(())
	}

	/// Resolve the avc1 config from an avcC.
	///
	/// The first config is stored. A different avcC would mean a new init
	/// segment, which a single fixed track can't represent, so a reconfiguration
	/// is an error (mint a new track via a fresh importer).
	fn set_config(&mut self, config: hang::catalog::VideoConfig) -> Result<()> {
		if let Some(old) = &self.config {
			if old == &config {
				return Ok(());
			}
			return Err(Error::FixedTrackReconfigured.into());
		}
		self.apply_config(config);
		Ok(())
	}

	fn apply_config(&mut self, config: hang::catalog::VideoConfig) {
		tracing::debug!(?config, "starting H.264 track");
		self.catalog
			.video
			.renditions
			.insert(self.track.name().to_string(), config.clone());
		self.config = Some(config);
	}

	/// Write split frames to the track, resolving the avc3 config from the first
	/// keyframe's inline SPS and refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			// avc1 config arrives out-of-band via initialize(); avc3 (and the
			// not-yet-resolved Pending case) carries SPS inline on keyframes.
			if !matches!(self.shape, Shape::Avc1 { .. })
				&& frame.keyframe
				&& let Some(sps) = find_sps(&frame.payload)
			{
				self.configure_from_sps(&sps)?;
			}

			if self.config.is_none() {
				// A keyframe we still can't configure is undecodable, so bail
				// loudly. A non-keyframe before config is a mid-stream-join
				// leftover: write it and let the producer's lenient start drop it
				// ahead of the first keyframe.
				if frame.keyframe {
					return Err(Error::NotInitialized.into());
				}
			}

			let pts = frame.timestamp;
			self.track.write(frame)?;

			if let Some(jitter) = self.jitter.observe(pts)
				&& let Some(c) = self.catalog.video.renditions.get_mut(self.track.name())
			{
				c.jitter = Some(jitter);
			}
		}
		Ok(())
	}
}

impl FrameDecode for Import {
	fn decode<I: IntoIterator<Item = Frame>>(&mut self, frames: I) -> Result<()> {
		self.write_frames(frames)
	}
}

impl Renditions for Import {
	fn renditions(&self) -> &hang::Catalog {
		&self.catalog
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
) -> Result<Frame> {
	let data = buf.as_ref();
	let pts = pts.ok_or(Error::MissingTimestamp)?;
	let keyframe = avc1_is_keyframe(data, length_size);
	let frame = Frame {
		timestamp: pts,
		payload: data.to_vec().into(),
		keyframe,
		duration: None,
	};
	buf.advance(buf.remaining());
	Ok(frame)
}

fn is_sps(nal: &[u8]) -> bool {
	nal.first().is_some_and(|h| h & 0x1f == NAL_TYPE_SPS)
}

/// Find the first SPS NAL in an Annex-B payload, if any.
fn find_sps(payload: &[u8]) -> Option<Bytes> {
	let mut buf = Bytes::copy_from_slice(payload);
	let mut nals = NalIterator::new(&mut buf);
	while let Some(Ok(nal)) = nals.next() {
		if is_sps(&nal) {
			return Some(nal);
		}
	}
	nals.flush().ok().flatten().filter(|nal| is_sps(nal))
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

#[cfg(test)]
mod tests {
	use super::*;

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

	/// An avcC initializer routes into the avc1 path and resolves a config with
	/// the avcC stored as `description`.
	#[tokio::test(start_paused = true)]
	async fn initialize_avc1_lands_in_catalog() {
		let sps_nal = [0x67, 0x42, 0xc0, 0x1f];
		let mut avcc = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps_nal.len() as u8];
		avcc.extend_from_slice(&sps_nal);
		avcc.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]); // num_pps + pps

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let track = broadcast
			.create_track(
				"video",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let mut import = Import::from_track(track);
		let mut buf = bytes::BytesMut::from(avcc.as_slice());
		import.initialize(&mut buf).expect("initialize avc1");

		let cfg = import
			.catalog()
			.expect("catalog known after init")
			.video
			.renditions
			.get("video")
			.expect("rendition");
		let hang::catalog::VideoCodec::H264(h264) = &cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(!h264.inline, "avc1 source should land as inline=false");
		assert_eq!(h264.profile, 0x42);
		assert_eq!(h264.level, 0x1f);
		assert_eq!(cfg.description.as_ref().expect("description").as_ref(), avcc.as_slice());
	}

	/// An avc3 stream self-initializes: no `initialize`, the config is resolved
	/// from the SPS the splitter packages into the first keyframe.
	#[tokio::test(start_paused = true)]
	async fn avc3_self_initializes_from_first_keyframe() {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

		let mut annexb = bytes::BytesMut::new();
		for nal in [sps, pps, idr] {
			annexb.extend_from_slice(&[0, 0, 0, 1]);
			annexb.extend_from_slice(nal);
		}

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let track = broadcast
			.create_track(
				"video",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let mut import = Import::from_track(track);
		assert!(import.catalog().is_none(), "no config before any frame");

		import
			.decode_frame(&mut annexb, Some(moq_net::Timestamp::from_micros(0).unwrap()))
			.expect("decode keyframe");

		let cfg = import.catalog().expect("config after keyframe");
		let h264_cfg = cfg.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H264(h264) = &h264_cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(h264.inline, "avc3 source should land as inline=true");
		assert!(h264_cfg.description.is_none(), "avc3 has no out-of-band description");
		assert_eq!(h264.profile, sps[1]);
		assert_eq!(h264.level, sps[3]);
	}

	/// A keyframe that carries no SPS (and no avcC/seed to fall back on) is
	/// undecodable, so it's a hard error rather than an uncatalogued frame.
	#[tokio::test(start_paused = true)]
	async fn keyframe_without_sps_errors() {
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21]; // IDR slice, no inline SPS
		let mut annexb = bytes::BytesMut::new();
		annexb.extend_from_slice(&[0, 0, 0, 1]);
		annexb.extend_from_slice(idr);

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let track = broadcast
			.create_track(
				"video",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let mut import = Import::from_track(track);

		let err = import
			.decode_frame(&mut annexb, Some(moq_net::Timestamp::from_micros(0).unwrap()))
			.expect_err("an unconfigurable keyframe must error");
		assert!(matches!(err, crate::Error::H264(Error::NotInitialized)), "got {err:?}");
	}

	/// A non-keyframe before any config is a mid-stream-join leftover: it must
	/// not abort the import (the producer's lenient start drops it downstream).
	/// A non-keyframe before the first keyframe has no group to anchor it, so the
	/// producer surfaces MissingKeyframe (which a mid-stream join skips).
	#[tokio::test(start_paused = true)]
	async fn delta_before_init_reports_missing_keyframe() {
		let pslice: &[u8] = &[0x61, 0xe0, 0x12, 0x34]; // non-IDR slice
		let mut annexb = bytes::BytesMut::new();
		annexb.extend_from_slice(&[0, 0, 0, 1]);
		annexb.extend_from_slice(pslice);

		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let track = broadcast
			.create_track(
				"video",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let mut import = Import::from_track(track);

		let err = import
			.decode_frame(&mut annexb, Some(moq_net::Timestamp::from_micros(0).unwrap()))
			.expect_err("a delta before any keyframe must report MissingKeyframe");
		assert!(matches!(err, crate::Error::MissingKeyframe(_)), "got {err:?}");
		assert!(import.catalog().is_none(), "no config yet, so no catalog");
	}
}
