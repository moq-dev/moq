//! AV1 importer.
//!
//! Publishes raw AV1 (OBU-framed, inline sequence headers) on a single moq
//! track and resolves the catalog rendition. The codec config comes from the
//! sequence header the splitter packages into the first keyframe (scanned out of
//! the frame here), or from the one in an av1C record's `configOBUs` handed to
//! [`initialize`](Import::initialize). A keyframe that can't be configured is an
//! error; non-keyframes before the first config are written through to the
//! producer, which reports [`MissingKeyframe`](crate::container::MissingKeyframe)
//! for a mid-stream join. OBU byte parsing lives in [`Split`](super::Split); this type is a
//! pure frame publisher that whoever owns the split drives via [`decode`](Import::decode).

use bytes::Bytes;
use scuffle_av1::seq::SequenceHeaderObu;
use scuffle_av1::{ObuHeader, ObuType};

use super::Error;
use super::split::ObuIterator;
use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// A pure-publisher importer for AV1 with inline sequence headers.
///
/// Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from, and feed it
/// frames a [`Split`](super::Split) produced via [`decode`](Self::decode). The
/// catalog rendition fills in lazily once the config is known.
pub struct Import {
	track: crate::container::Producer<crate::catalog::hang::Container, hang::catalog::VideoConfig>,
	catalog: crate::codec::video::Catalog,
	last_seq: Option<Bytes>,
}

impl Import {
	/// Publish on an existing track producer, seeding the rendition from `hint` (pass
	/// [`VideoHint::default`](crate::catalog::VideoHint) for none).
	///
	/// A hint carrying a codec publishes the catalog rendition up front (the sequence header still
	/// refines it in band on the first keyframe).
	pub fn new<E: CatalogExt>(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		hint: crate::catalog::VideoHint,
	) -> crate::Result<Self> {
		// The hint names the container; the writer is built from that same value so the wire
		// cannot disagree with what the rendition advertises.
		let wire = crate::catalog::hang::Container::try_from(&hint)?;
		let catalog = crate::codec::video::Catalog::new(hint);
		let track = reserved.video(track, wire, None)?;
		let mut import = Self {
			track,
			catalog,
			last_seq: None,
		};
		if let Some(config) = import.catalog.initial_config() {
			import.apply_config(config)?;
		}
		Ok(import)
	}

	/// Resolve the codec config from a sequence header / av1C and other metadata.
	///
	/// - **av1C** (leading `0x81` marker): the buffer is parsed as an
	///   AV1CodecConfigurationRecord, whose `configOBUs` sequence header resolves the config.
	/// - **raw OBUs**: any sequence header resolves the config.
	///
	/// Optional, since the importer also self-initializes from the first keyframe.
	/// The buffer is *not* consumed: the dispatcher-owned [`Split`](super::Split)
	/// consumes it (seeding the sequence header so it prefixes the first keyframe).
	pub fn initialize(&mut self, buf: &[u8]) -> Result<()> {
		let data = buf;

		// av1C box starts with 0x81 (marker=1, version=1) per ISO/IEC 14496-15. Only the
		// fixed 4-byte header is read here, so don't gate on a larger size or a short
		// out-of-band record falls through to raw-OBU scanning and leaves the config unset.
		if data.len() >= 4 && data[0] == 0x81 {
			self.init_from_av1c(data)?;
			return Ok(());
		}

		// Raw OBUs: resolve the config from any sequence header. A new one starts a coded sequence,
		// so the last one's HDR metadata is dropped before the config publishes.
		if let Some(seq) = find_sequence_header(data) {
			if self.last_seq.as_ref() != Some(&seq) {
				self.catalog.hdr(&mut self.track, Default::default(), true)?;
			}
			self.configure_from_seq(&seq)?;
		}
		Ok(())
	}

	/// Resolve the config from the sequence header in an av1C's `configOBUs`.
	///
	/// The record's own header fields omit the colour the codec string carries, so a config built
	/// from them alone would differ from the stream's first sequence header and force a new track.
	/// A record without one publishes nothing: the first keyframe must carry it to be decodable.
	fn init_from_av1c(&mut self, data: &[u8]) -> crate::Result<()> {
		// configOBUs follow the four header bytes; samples need not repeat what they carry. A new
		// record starts a new coded sequence, whose HDR metadata is what its configOBUs carry.
		let obus = &data[4..];
		let hdr = find_hdr(&Bytes::copy_from_slice(obus));
		self.catalog.hdr(&mut self.track, hdr, true)?;
		if let Some(seq) = find_sequence_header(obus) {
			self.configure_from_seq(&seq)?;
		}
		Ok(())
	}

	fn init(&mut self, seq_header: &SequenceHeaderObu) -> crate::Result<()> {
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
		config.color = sequence_color(seq_header);
		self.apply_config(config)?;
		Ok(())
	}

	/// Minimal config when sequence-header parsing fails, so the stream can still
	/// flow (the catalog just won't carry full codec info).
	fn init_minimal(&mut self) -> crate::Result<()> {
		let config = hang::catalog::VideoConfig::new(hang::catalog::AV1 {
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
		self.apply_config(config)?;
		Ok(())
	}

	/// Apply a resolved config, minting a track when the decoder identity changes.
	fn apply_config(&mut self, config: hang::catalog::VideoConfig) -> crate::Result<()> {
		self.catalog.publish(&mut self.track, config)
	}

	/// Resolve the config from a sequence-header OBU, falling back to a minimal
	/// config if it fails to parse.
	fn configure_from_seq(&mut self, seq_obu: &Bytes) -> Result<()> {
		if self.last_seq.as_ref() == Some(seq_obu) {
			return Ok(());
		}
		// Own just the sequence header: the temporal unit it was found in is far larger.
		self.last_seq = Some(Bytes::copy_from_slice(seq_obu));

		match parse_sequence_header(seq_obu)? {
			Some(seq_header) => self.init(&seq_header)?,
			None if !self.catalog.configured() => {
				tracing::debug!("sequence header parse failed, using minimal config");
				self.init_minimal()?;
			}
			None => {}
		}
		Ok(())
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`]. Consumes this importer.
	pub fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}

	/// Cut the current group at `end` without finishing the track.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> Result<()> {
		self.track.cut(end)?;
		Ok(())
	}

	/// Mark a timeline break and restart measurement without lowering advertised values.
	pub fn discontinuity(&mut self) -> crate::Result<()> {
		self.track.discontinuity()
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
			if frame.keyframe {
				let seq = find_sequence_header(&frame.payload);
				// A new sequence header starts a coded sequence whose HDR metadata replaces the last
				// one's. It goes first, so a keyframe that also configures publishes once.
				// The first header after an av1C is the record's own sequence, which sync samples repeat.
				let sequence = seq
					.as_ref()
					.is_some_and(|seq| self.last_seq.as_ref().is_some_and(|last| last != seq));
				self.catalog.hdr(&mut self.track, find_hdr(&frame.payload), sequence)?;
				if let Some(seq) = seq {
					self.configure_from_seq(&seq)?;
				}
			}

			// A keyframe we couldn't configure (no sequence header) is undecodable.
			if frame.keyframe && !self.catalog.configured() {
				return Err(Error::MissingSequenceHeader.into());
			}

			// A pre-keyframe delta has no group to anchor it: the producer returns
			// MissingKeyframe, which a caller joining mid-stream skips.
			self.track.write(frame)?;
		}
		Ok(())
	}

	/// Publish split frames, resolving the config from the first keyframe's inline
	/// sequence header and refining the catalog jitter.
	pub fn decode(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		self.write_frames(frames)
	}
}

fn is_sequence_header(obu: &[u8]) -> bool {
	let mut reader = obu;
	ObuHeader::parse(&mut reader)
		.map(|h| h.obu_type == ObuType::SequenceHeader)
		.unwrap_or(false)
}

fn parse_sequence_header(obu: &[u8]) -> Result<Option<SequenceHeaderObu>> {
	let mut reader = obu;
	let header = ObuHeader::parse(&mut reader)?;
	Ok(SequenceHeaderObu::parse(header, &mut reader).ok())
}

/// The colour a sequence header's `color_config` signals; its range is always explicit.
fn sequence_color(seq_header: &SequenceHeaderObu) -> Option<hang::catalog::Color> {
	let color = &seq_header.color_config;
	crate::codec::color::cicp(
		color.color_primaries,
		color.transfer_characteristics,
		color.matrix_coefficients,
		Some(color.full_color_range),
	)
}

/// The colour a run of OBUs signals, such as av1C's `configOBUs`: the first parseable sequence
/// header's, plus the static HDR metadata OBUs that the AV1 ISOBMFF binding places after it.
pub(crate) fn color(obus: &[u8]) -> Option<hang::catalog::Color> {
	let mut color = find_sequence_header(obus)
		.and_then(|seq| parse_sequence_header(&seq).ok().flatten())
		.and_then(|seq| sequence_color(&seq));
	find_hdr(&Bytes::copy_from_slice(obus)).apply(&mut color);
	color
}

/// Read encoded dimensions from the first parseable sequence header in a frame.
pub(crate) fn dimensions(payload: &[u8]) -> Result<Option<(u32, u32)>> {
	let Some(sequence) = find_sequence_header(payload) else {
		return Ok(None);
	};
	let Some(sequence) = parse_sequence_header(&sequence)? else {
		return Ok(None);
	};
	Ok(Some((
		sequence.max_frame_width as u32,
		sequence.max_frame_height as u32,
	)))
}

/// The HDR metadata a temporal unit's metadata OBUs carry.
fn find_hdr(payload: &Bytes) -> crate::codec::color::Hdr {
	let mut hdr = crate::codec::color::Hdr::default();
	let mut scan = |obu: &[u8]| {
		let mut reader = obu;
		if let Ok(header) = ObuHeader::parse(&mut reader)
			&& header.obu_type == ObuType::Metadata
		{
			hdr.update(crate::codec::color::av1_metadata(reader));
		}
	};
	let mut buf = payload.clone();
	let mut obus = ObuIterator::new(&mut buf);
	while let Some(Ok(obu)) = obus.next() {
		scan(&obu);
	}
	if let Ok(Some(obu)) = obus.flush() {
		scan(&obu);
	}
	hdr
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

#[cfg(test)]
mod tests {
	use super::*;

	/// A metadata OBU (type 5, with a size field) carrying `payload`.
	fn metadata_obu(payload: &[u8]) -> Vec<u8> {
		let mut obu = vec![(5 << 3) | 0x02, payload.len() as u8];
		obu.extend_from_slice(payload);
		obu
	}

	/// HDR10 metadata OBUs: a 1000-nit BT.2020 mastering display, then MaxCLL 1000 and MaxFALL 400.
	fn hdr10_obus() -> Vec<u8> {
		let mut mdcv = vec![2];
		for v in [46_399u16, 19_136, 11_141, 52_232, 8_585, 3_014, 20_493, 21_561] {
			mdcv.extend_from_slice(&v.to_be_bytes());
		}
		mdcv.extend_from_slice(&(1000u32 << 8).to_be_bytes());
		mdcv.extend_from_slice(&82u32.to_be_bytes());
		let mut obus = metadata_obu(&mdcv);
		obus.extend(metadata_obu(&[1, 0x03, 0xE8, 0x01, 0x90]));
		obus
	}

	/// An av1C's configOBUs may carry the static HDR metadata alone; it is not dropped.
	#[test]
	fn config_obus_carry_hdr_metadata() {
		let color = color(&hdr10_obus()).expect("HDR metadata");
		assert_eq!(color.mastering.map(|m| m.max_luminance), Some(1000.0));
		assert_eq!(color.content_light, crate::codec::color::test::hdr10().content_light);
		assert_eq!(color.primaries, None);
	}

	/// A track initialized from an av1C publishes the colour its configOBUs carry, since the
	/// samples need not repeat it, and the first sync sample repeating its sequence header keeps
	/// the track.
	#[tokio::test(start_paused = true)]
	async fn av1c_initialize_publishes_config_obu_color() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();

		let mut av1c = vec![0x81, 0x09, 0x4c, 0x00];
		av1c.extend(av1_config_obus());
		av1c.extend(hdr10_obus());
		import.initialize(&av1c).unwrap();

		let color = catalog.snapshot().video.renditions["video"]
			.color
			.clone()
			.expect("colour");
		assert_eq!(color.content_light, crate::codec::color::test::hdr10().content_light);
		assert!(color.mastering.is_some());

		// The first sync sample repeats a sequence header but not the metadata OBUs.
		let sequence = av1_sequence_header();
		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut split = super::super::Split::new();
		let mut frames = split.decode(&sequence, pts).unwrap();
		frames.extend(split.flush(pts).unwrap());
		import.decode(frames).unwrap();
		assert_eq!(
			import.track.name(),
			"video",
			"the repeated sequence header keeps the track"
		);
		let color = catalog.snapshot().video.renditions["video"]
			.color
			.clone()
			.expect("colour");
		assert!(
			color.mastering.is_some(),
			"the av1C's HDR metadata survives the first sync sample"
		);
	}

	/// An av1C without a sequence header publishes nothing: its header fields omit the colour the
	/// codec string carries, so the stream's first sequence header configures the track instead.
	#[tokio::test(start_paused = true)]
	async fn av1c_without_a_sequence_header_waits_for_the_stream() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		import.initialize(&[0x81, 0x09, 0x4c, 0x00]).unwrap();
		assert!(catalog.snapshot().video.renditions.is_empty());

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut split = super::super::Split::new();
		let mut frames = split.decode(&av1_sequence_header(), pts).unwrap();
		frames.extend(split.flush(pts).unwrap());
		import.decode(frames).unwrap();
		assert_eq!(import.track.name(), "video");
		assert_eq!(catalog.snapshot().video.renditions["video"].coded_width, Some(1920));
	}

	/// Seeding with raw OBUs whose sequence header is new starts a new coded sequence, dropping the
	/// HDR metadata an earlier av1C carried.
	#[tokio::test(start_paused = true)]
	async fn a_new_raw_sequence_drops_stale_hdr_metadata() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		let mut av1c = vec![0x81, 0x08, 0x0c, 0x00];
		av1c.extend(hdr10_obus());
		import.initialize(&av1c).unwrap();

		import.initialize(&av1_sequence_header()).unwrap();
		let color = catalog.snapshot().video.renditions["video"].color.clone().unwrap();
		assert_eq!((color.mastering, color.content_light), (None, None));
	}

	/// The configOBUs of `av1.mp4`'s av1C: its sequence header.
	fn av1_config_obus() -> Vec<u8> {
		let init = include_bytes!("../../container/fmp4/test_data/av1.mp4");
		let mut cursor = std::io::Cursor::new(&init[..]);
		let moov = loop {
			match <mp4_atom::Any as mp4_atom::Decode>::decode(&mut cursor).unwrap() {
				mp4_atom::Any::Moov(moov) => break moov,
				_ => continue,
			}
		};
		let mp4_atom::Codec::Av01(av01) = &moov.trak[0].mdia.minf.stbl.stsd.codecs[0] else {
			panic!("expected av01");
		};
		av01.av1c.config_obus.clone()
	}

	/// A temporal unit of a temporal delimiter, the sequence header from `av1.mp4`'s av1C, and a frame.
	fn av1_sequence_header() -> Vec<u8> {
		let mut tu = vec![0x12, 0x00];
		tu.extend(av1_config_obus());
		tu.extend_from_slice(&[0x32, 0x01, 0x10]);
		tu
	}
}
