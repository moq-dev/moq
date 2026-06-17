//! AV1 importer.
//!
//! Publishes raw AV1 (OBU-framed, inline sequence headers) on a single moq
//! track and resolves the catalog rendition. The codec config comes from the
//! sequence header the splitter packages into the first keyframe (scanned out of
//! the frame here), or from an av1C record handed to
//! [`initialize`](Import::initialize). A keyframe that can't be configured is an
//! error; non-keyframes before the first config are written through to the
//! producer, which reports [`MissingKeyframe`](crate::container::MissingKeyframe)
//! for a mid-stream join. OBU byte parsing lives in [`Split`](super::Split); this type is a
//! pure frame publisher that whoever owns the split drives via the
//! [`FrameDecode`] trait.

use bytes::{Buf, Bytes};
use scuffle_av1::seq::SequenceHeaderObu;
use scuffle_av1::{ObuHeader, ObuType};

use super::Error;
use super::split::ObuIterator;
use crate::Result;
use crate::container::Frame;
use crate::container::jitter::MinFrameDuration;
use crate::import::{FrameDecode, Renditions};

/// A pure-publisher importer for AV1 with inline sequence headers.
///
/// Build it from a [`moq_net::TrackRequest`] ([`new`](Self::new)) or an existing
/// track ([`from_track`](Self::from_track)), and feed it frames a [`Split`](super::Split)
/// produced via the [`FrameDecode`] impl. The catalog rendition fills in lazily
/// once the config is known; read it via [`catalog`](Self::catalog).
pub struct Import {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: hang::Catalog,
	config: Option<hang::catalog::VideoConfig>,
	last_seq: Option<Bytes>,
	jitter: MinFrameDuration,
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
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog: hang::Catalog::default(),
			config: None,
			last_seq: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Resolve the codec config from a sequence header / av1C and other metadata.
	///
	/// - **av1C** (leading `0x81` marker): the buffer is parsed as an
	///   AV1CodecConfigurationRecord, which resolves the config.
	/// - **raw OBUs**: any sequence header resolves the config.
	///
	/// Optional, since the importer also self-initializes from the first keyframe.
	/// The buffer is *not* consumed: the dispatcher-owned [`Split`](super::Split)
	/// consumes it (seeding the sequence header so it prefixes the first keyframe).
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let data = buf.as_ref();

		// av1C box starts with 0x81 (marker=1, version=1) per ISO/IEC 14496-15.
		if data.len() >= 16 && data[0] == 0x81 {
			self.init_from_av1c(data)?;
			return Ok(());
		}

		// Raw OBUs: resolve the config from any sequence header.
		if let Some(seq) = find_sequence_header(data) {
			self.configure_from_seq(&seq)?;
		}
		Ok(())
	}

	fn init_from_av1c(&mut self, data: &[u8]) -> Result<()> {
		let seq_profile = (data[1] >> 5) & 0x07;
		let seq_level_idx = data[1] & 0x1F;
		let tier = ((data[2] >> 7) & 0x01) == 1;
		let high_bitdepth = ((data[2] >> 6) & 0x01) == 1;
		let twelve_bit = ((data[2] >> 5) & 0x01) == 1;

		// Resolution is unknown from av1C; it's filled when the first sequence header arrives.
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
			profile: seq_profile,
			level: seq_level_idx,
			tier: if tier { 'H' } else { 'M' },
			bitdepth: super::bitdepth(twelve_bit, high_bitdepth),
			mono_chrome: ((data[2] >> 4) & 0x01) == 1,
			chroma_subsampling_x: ((data[2] >> 3) & 0x01) == 1,
			chroma_subsampling_y: ((data[2] >> 2) & 0x01) == 1,
			chroma_sample_position: data[2] & 0x03,
			color_primaries: 1,
			transfer_characteristics: 1,
			matrix_coefficients: 1,
			full_range: false,
		});
		config.container = hang::catalog::Container::Legacy;
		self.apply_config(config);
		Ok(())
	}

	fn init(&mut self, seq_header: &SequenceHeaderObu) -> Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
			profile: seq_header.seq_profile,
			level: seq_header
				.operating_points
				.first()
				.map(|op| op.seq_level_idx)
				.unwrap_or(0),
			tier: if seq_header
				.operating_points
				.first()
				.map(|op| op.seq_tier)
				.unwrap_or(false)
			{
				'H'
			} else {
				'M'
			},
			bitdepth: seq_header.color_config.bit_depth as u8,
			mono_chrome: seq_header.color_config.mono_chrome,
			chroma_subsampling_x: seq_header.color_config.subsampling_x,
			chroma_subsampling_y: seq_header.color_config.subsampling_y,
			chroma_sample_position: seq_header.color_config.chroma_sample_position,
			color_primaries: seq_header.color_config.color_primaries,
			transfer_characteristics: seq_header.color_config.transfer_characteristics,
			matrix_coefficients: seq_header.color_config.matrix_coefficients,
			full_range: seq_header.color_config.full_color_range,
		});
		config.coded_width = Some(seq_header.max_frame_width as u32);
		config.coded_height = Some(seq_header.max_frame_height as u32);
		config.container = hang::catalog::Container::Legacy;
		self.apply_config(config);
		Ok(())
	}

	/// Minimal config when sequence-header parsing fails, so the stream can still
	/// flow (the catalog just won't carry full codec info).
	fn init_minimal(&mut self) -> Result<()> {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
			profile: 0,
			level: 0,
			tier: 'M',
			bitdepth: 8,
			mono_chrome: false,
			chroma_subsampling_x: true, // 4:2:0
			chroma_subsampling_y: true,
			chroma_sample_position: 0,
			color_primaries: 2,          // Unspecified
			transfer_characteristics: 2, // Unspecified
			matrix_coefficients: 2,      // Unspecified
			full_range: false,
		});
		config.container = hang::catalog::Container::Legacy;
		self.apply_config(config);
		Ok(())
	}

	/// Apply a resolved config, updating the catalog rendition in place.
	///
	/// A changed config just re-mirrors the rendition; there are no fixed tracks
	/// to reject a reconfiguration.
	fn apply_config(&mut self, config: hang::catalog::VideoConfig) {
		if self.config.as_ref() == Some(&config) {
			return;
		}
		tracing::debug!(name = ?self.track.name(), ?config, "starting track");
		self.catalog
			.video
			.renditions
			.insert(self.track.name().to_string(), config.clone());
		self.config = Some(config);
	}

	/// Resolve the config from a sequence-header OBU, falling back to a minimal
	/// config if it fails to parse.
	fn configure_from_seq(&mut self, seq_obu: &Bytes) -> Result<()> {
		if self.last_seq.as_ref() == Some(seq_obu) {
			return Ok(());
		}
		self.last_seq = Some(seq_obu.clone());

		let mut reader = &seq_obu[..];
		let header = ObuHeader::parse(&mut reader)?;
		let payload_offset = seq_obu.len() - reader.len();

		match SequenceHeaderObu::parse(header, &mut &seq_obu[payload_offset..]) {
			Ok(seq_header) => self.init(&seq_header),
			Err(_) if self.config.is_none() => {
				tracing::debug!("sequence header parse failed, using minimal config");
				self.init_minimal()
			}
			Err(_) => Ok(()),
		}
	}

	/// The standalone catalog once the config is known, else `None`.
	pub fn catalog(&self) -> Option<&hang::Catalog> {
		self.config.is_some().then_some(&self.catalog)
	}

	/// The underlying track producer.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
	}

	/// True once the config is known and the catalog has been populated.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Write split frames to the track, resolving the config from the first
	/// keyframe's inline sequence header and refining the catalog jitter.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			if frame.keyframe
				&& let Some(seq) = find_sequence_header(&frame.payload)
			{
				self.configure_from_seq(&seq)?;
			}

			// A keyframe we couldn't configure (no sequence header) is undecodable.
			if frame.keyframe && self.config.is_none() {
				return Err(Error::MissingSequenceHeader.into());
			}

			let pts = frame.timestamp;
			// A pre-keyframe delta has no group to anchor it: the producer returns
			// MissingKeyframe, which a caller joining mid-stream skips.
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

fn is_sequence_header(obu: &[u8]) -> bool {
	let mut reader = obu;
	ObuHeader::parse(&mut reader)
		.map(|h| h.obu_type == ObuType::SequenceHeader)
		.unwrap_or(false)
}

/// Find the first sequence-header OBU in a payload, if any.
fn find_sequence_header(payload: &[u8]) -> Option<Bytes> {
	let mut buf = Bytes::copy_from_slice(payload);
	let mut obus = ObuIterator::new(&mut buf);
	while let Some(Ok(obu)) = obus.next() {
		if is_sequence_header(&obu) {
			return Some(obu);
		}
	}
	obus.flush().ok().flatten().filter(|obu| is_sequence_header(obu))
}
