//! H.265 importer.
//!
//! Publishes H.265 frames on a single moq track and resolves the catalog
//! rendition, in either wire shape: Annex-B with inline VPS/SPS/PPS ("hev1"), or
//! length-prefixed NALU with an out-of-band hvcC ("hvc1"). Only single-layer
//! streams are supported (VPS is cached but not parsed).
//!
//! The codec config comes from exactly one of two places: an hvcC handed to
//! [`initialize`](Import::initialize) (the hvc1 shape), or the SPS the splitter
//! packages into the first keyframe (hev1). A keyframe
//! that can't be configured is an error; non-keyframes before the first config
//! are written through to the producer, which reports
//! [`MissingKeyframe`](crate::container::MissingKeyframe) for a mid-stream join.
//! Annex-B byte parsing lives in [`Split`](super::Split); this type is a pure frame publisher
//! that whoever owns the split drives via [`decode`](Import::decode).

use bytes::Bytes;
use scuffle_h265::SpsNALUnit;

use super::{Error, split::nal_unit_type};
use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::codec::annexb::NalIterator;
use crate::container::Frame;

/// A pure-publisher importer for H.265.
/// Only supports single layer streams (VPS is cached but not parsed).
///
/// Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from, and feed it
/// frames a [`Split`](super::Split) produced via [`decode`](Self::decode). The
/// catalog rendition fills in lazily once the codec config is known (hvcC via
/// [`initialize`](Self::initialize) for hvc1, the first SPS for hev1).
pub struct Import<E: CatalogExt = ()> {
	/// True for the hvc1 shape: the codec config is out-of-band (hvcC), so
	/// frame payloads are length-prefixed rather than Annex-B and are never scanned.
	hvc1: bool,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::VideoTrack<E>,
	catalog: crate::codec::video::Catalog,
	last_sps: Option<Bytes>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, seeding the rendition from `hint` (pass
	/// [`VideoHint::default`](crate::catalog::VideoHint) for none).
	///
	/// A hint carrying a codec publishes the catalog rendition up front (the VPS/SPS/PPS still refine
	/// it in band on the first keyframe).
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		hint: crate::catalog::VideoHint,
	) -> crate::Result<Self> {
		let rendition = reserved.video(track.name());
		let catalog = crate::codec::video::Catalog::new(&reserved, track.name(), hint)?;
		let mut import = Self {
			hvc1: false,
			track: reserved
				.producer()
				.media_producer(track, crate::catalog::hang::Container::Legacy)?,
			rendition,
			catalog,
			last_sps: None,
		};
		if let Some(config) = import.catalog.initial_config() {
			import.apply_config(config);
		}
		Ok(import)
	}

	/// Resolve the codec config from the codec's leading bytes.
	///
	/// - **hvc1** (no leading start code): parsed as an `HEVCDecoderConfigurationRecord`,
	///   which resolves the config and is stored as the catalog `description`. Required
	///   for hvc1.
	/// - **hev1** (leading start code): parsed as Annex-B; any SPS resolves the config.
	///   Optional, since hev1 also self-initializes from the first keyframe.
	///
	/// Takes a read-only slice: the dispatcher-owned [`Split`](super::Split) is what
	/// consumes the stream (and reads the same hvcC for the NALU length size). The
	/// shape is detected from the leading bytes.
	pub fn initialize(&mut self, buf: &[u8]) -> Result<()> {
		if crate::codec::annexb::is_config_record(buf) {
			self.initialize_hvc1(buf)
		} else {
			self.initialize_hev1(buf)
		}
	}

	fn initialize_hvc1(&mut self, hvcc_bytes: &[u8]) -> Result<()> {
		// Only switch to hvc1 mode once the hvcC actually parses, so a parse failure leaves the
		// importer in hev1 mode where inline-SPS keyframes still self-initialize.
		let config = super::config_from_hvcc(hvcc_bytes)?;
		self.hvc1 = true;
		self.apply_config(config);
		Ok(())
	}

	/// Resolve the config from any SPS in the buffer.
	fn initialize_hev1(&mut self, buf: &[u8]) -> Result<()> {
		let mut scan = Bytes::copy_from_slice(buf);
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
		Ok(())
	}

	/// The MoQ track name this importer publishes on.
	pub fn name(&self) -> &str {
		self.track.name()
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> Result<()> {
		self.rendition.record_group_end(None);
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
		self.rendition.record_group_end(end);
		self.track.cut(end)?;
		Ok(())
	}

	/// Mark a break in the timeline by publishing an empty group. To bound the closing
	/// group's final frame first, [`cut(end)`](Self::cut) before this. See
	/// [`Producer::discontinuity`](crate::container::Producer::discontinuity).
	pub fn discontinuity(&mut self) -> Result<()> {
		self.rendition.record_group_end(None);
		self.track.discontinuity()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.rendition.record_group_end(None);
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Record a frame's reorder delay (`PTS - DTS`) so the catalog `jitter` reflects the
	/// B-frame reorder depth (the decode buffer a transmuxer/player must hold). The
	/// container supplies this since the elementary stream alone carries no decode time.
	pub fn observe_reorder(&mut self, reorder: moq_net::Timestamp) {
		self.rendition.record_reorder(reorder);
	}

	/// Resolve the config from an inline SPS, updating the rendition in place on a
	/// change.
	fn configure_from_sps(&mut self, sps_nal: &Bytes) -> Result<()> {
		if self.last_sps.as_ref() == Some(sps_nal) {
			return Ok(());
		}

		let sps = SpsNALUnit::parse(&mut &sps_nal[..]).map_err(|_| Error::SpsParse)?;
		let profile = &sps.rbsp.profile_tier_level.general_profile;
		let vui_data = sps.rbsp.vui_parameters.as_ref().map(VuiData::new).unwrap_or_default();

		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H265 {
			in_band: true, // An inline SPS is the hev1 shape; hvc1 configs come from `config_from_hvcc`.
			profile_space: profile.profile_space,
			profile_idc: profile.profile_idc,
			profile_compatibility_flags: profile.profile_compatibility_flag.bits().to_be_bytes(),
			tier_flag: profile.tier_flag,
			level_idc: profile.level_idc.ok_or(Error::MissingLevelIdc)?,
			constraint_flags: super::pack_constraint_flags(profile),
		});
		config.coded_width = Some(sps.rbsp.cropped_width() as u32);
		config.coded_height = Some(sps.rbsp.cropped_height() as u32);
		config.framerate = vui_data.framerate;
		config.display_aspect_width = vui_data.display_ratio_width;
		config.display_aspect_height = vui_data.display_ratio_height;
		config.container = hang::catalog::Container::Legacy;

		self.last_sps = Some(sps_nal.clone());
		self.apply_config(config);
		Ok(())
	}

	/// Apply a resolved config, updating the catalog rendition in place.
	///
	/// A changed config just re-mirrors the rendition; there are no fixed tracks to reject a
	/// reconfiguration.
	fn apply_config(&mut self, config: hang::catalog::VideoConfig) {
		self.catalog.publish(&mut self.rendition, config);
	}

	/// Write split frames to the track, resolving the config from the first
	/// keyframe's inline SPS and refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			// hvc1 config arrives out-of-band via initialize(); hev1 carries SPS inline on
			// keyframes. Scanning an hvc1 payload would misread a length prefix as a start
			// code (a 322-byte NAL prefixes as `00 00 01 42`, an SPS NAL header).
			if !self.hvc1
				&& frame.keyframe
				&& let Some(sps) = find_sps(&frame.payload)
			{
				self.configure_from_sps(&sps)?;
			}

			// A keyframe we still can't configure (no SPS) is undecodable.
			if frame.keyframe && !self.catalog.configured() {
				return Err(Error::MissingSps.into());
			}

			// A keyframe starts a new group: close the previous one for the bitrate detector.
			if frame.keyframe {
				self.rendition.record_group_end(Some(frame.timestamp));
			}

			let pts = frame.timestamp;
			let bytes = frame.payload.len();
			// A pre-keyframe delta has no group to anchor it: the producer returns
			// MissingKeyframe, which the caller (e.g. a TS mid-stream join) skips.
			self.track.write(frame)?;

			self.rendition.record_frame(pts, bytes);
		}
		Ok(())
	}

	/// Publish split frames, resolving the config from the first keyframe's inline
	/// SPS and refining the catalog jitter as it goes.
	pub fn decode(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		self.write_frames(frames)
	}
}

fn is_sps(nal: &[u8]) -> bool {
	nal.first()
		.is_some_and(|h| nal_unit_type(*h) == scuffle_h265::NALUnitType::SpsNut)
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

#[derive(Default)]
struct VuiData {
	framerate: Option<f64>,
	display_ratio_width: Option<u32>,
	display_ratio_height: Option<u32>,
}

impl VuiData {
	fn new(vui: &scuffle_h265::VuiParameters) -> Self {
		// FPS = time_scale / num_units_in_tick
		let framerate = vui
			.vui_timing_info
			.as_ref()
			.map(|t| t.time_scale.get() as f64 / t.num_units_in_tick.get() as f64);

		let (display_ratio_width, display_ratio_height) = match &vui.aspect_ratio_info {
			// Extended SAR has explicit arbitrary values for width and height.
			scuffle_h265::AspectRatioInfo::ExtendedSar { sar_width, sar_height } => {
				(Some(*sar_width as u32), Some(*sar_height as u32))
			}
			// Predefined map to known values.
			scuffle_h265::AspectRatioInfo::Predefined(idc) => aspect_ratio_from_idc(*idc)
				.map(|(w, h)| (Some(w), Some(h)))
				.unwrap_or((None, None)),
		};

		VuiData {
			framerate,
			display_ratio_width,
			display_ratio_height,
		}
	}
}

fn aspect_ratio_from_idc(idc: scuffle_h265::AspectRatioIdc) -> Option<(u32, u32)> {
	match idc {
		scuffle_h265::AspectRatioIdc::Unspecified => None,
		scuffle_h265::AspectRatioIdc::Square => Some((1, 1)),
		scuffle_h265::AspectRatioIdc::Aspect12_11 => Some((12, 11)),
		scuffle_h265::AspectRatioIdc::Aspect10_11 => Some((10, 11)),
		scuffle_h265::AspectRatioIdc::Aspect16_11 => Some((16, 11)),
		scuffle_h265::AspectRatioIdc::Aspect40_33 => Some((40, 33)),
		scuffle_h265::AspectRatioIdc::Aspect24_11 => Some((24, 11)),
		scuffle_h265::AspectRatioIdc::Aspect20_11 => Some((20, 11)),
		scuffle_h265::AspectRatioIdc::Aspect32_11 => Some((32, 11)),
		scuffle_h265::AspectRatioIdc::Aspect80_33 => Some((80, 33)),
		scuffle_h265::AspectRatioIdc::Aspect18_11 => Some((18, 11)),
		scuffle_h265::AspectRatioIdc::Aspect15_11 => Some((15, 11)),
		scuffle_h265::AspectRatioIdc::Aspect64_33 => Some((64, 33)),
		scuffle_h265::AspectRatioIdc::Aspect160_99 => Some((160, 99)),
		scuffle_h265::AspectRatioIdc::Aspect4_3 => Some((4, 3)),
		scuffle_h265::AspectRatioIdc::Aspect3_2 => Some((3, 2)),
		scuffle_h265::AspectRatioIdc::Aspect2_1 => Some((2, 1)),
		scuffle_h265::AspectRatioIdc::ExtendedSar => None,
		_ => None, // Reserved
	}
}

#[cfg(test)]
mod tests {
	use bytes::BytesMut;

	use super::*;
	use crate::codec::h265::{Split, fixtures};

	fn setup(name: &str) -> (moq_net::track::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let track = broadcast.create_track(name, hang::container::track_info()).unwrap();
		(track, catalog)
	}

	/// An hvcC initializer resolves a config with the hvcC stored as `description`.
	/// The hvc1 analogue of the avcC test in [`crate::codec::h264`]'s importer.
	#[tokio::test(start_paused = true)]
	async fn initialize_hvcc_lands_in_catalog() {
		let hvcc = fixtures::hvcc();

		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		// initialize() must not consume the buffer (the dispatcher reads the same
		// hvcC for the NALU length size).
		let buf = BytesMut::from(hvcc.as_ref());
		import.initialize(&buf).expect("initialize hvcC");
		assert_eq!(buf.len(), hvcc.len(), "initialize must not consume the buffer");

		let snapshot = catalog.snapshot();
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H265(h265) = &cfg.codec else {
			panic!("expected H.265 codec")
		};
		assert!(!h265.in_band, "hvc1 source should land as in_band=false");
		assert_eq!(cfg.coded_width, Some(1280));
		assert_eq!(cfg.coded_height, Some(720));
		assert_eq!(cfg.description.as_deref(), Some(hvcc.as_ref()));
	}

	/// An hvc1 payload is never scanned for an inline SPS. A NAL of exactly 322 bytes
	/// carries the 4-byte length prefix `00 00 01 42`, which the Annex-B scanner reads
	/// as a 3-byte start code followed by an SPS NAL header (0x42 is type 33), so
	/// scanning would fail the parse and reject a perfectly valid keyframe.
	#[tokio::test(start_paused = true)]
	async fn hvc1_length_prefix_is_not_scanned_as_annexb() {
		let hvcc = fixtures::hvcc();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		import.initialize(&hvcc).expect("initialize hvcC");

		let mut nal = vec![0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
		nal.resize(322, 0x80);
		let mut au = Vec::new();
		au.extend_from_slice(&(nal.len() as u32).to_be_bytes());
		au.extend_from_slice(&nal);
		assert_eq!(
			&au[..4],
			&[0, 0, 1, 0x42],
			"the length prefix must look like a start code"
		);

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let frame = crate::codec::h265::hvc1_frame(&au, 4, pts).unwrap();
		assert!(frame.keyframe);
		import.decode([frame]).expect("hvc1 keyframe");

		// The out-of-band config survives: a scan would have overwritten it with in_band=true.
		let snapshot = catalog.snapshot();
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H265(h265) = &cfg.codec else {
			panic!("expected H.265 codec")
		};
		assert!(!h265.in_band);
		assert_eq!(cfg.description.as_deref(), Some(hvcc.as_ref()));
	}

	/// A hev1 stream self-initializes: the config is resolved from the SPS the
	/// splitter packages into the first keyframe.
	#[tokio::test(start_paused = true)]
	async fn hev1_self_initializes_from_first_keyframe() {
		let idr: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
		let mut annexb = BytesMut::new();
		for nal in [fixtures::VPS, fixtures::SPS, fixtures::PPS, idr] {
			annexb.extend_from_slice(&[0, 0, 0, 1]);
			annexb.extend_from_slice(nal);
		}

		let mut split = Split::new();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		assert!(
			catalog.snapshot().video.renditions.is_empty(),
			"no config before any frame"
		);

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&annexb, pts).expect("split keyframe");
		frames.extend(split.flush(pts).expect("flush keyframe"));
		import.decode(frames).expect("decode keyframe");

		let snapshot = catalog.snapshot();
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H265(h265) = &cfg.codec else {
			panic!("expected H.265 codec")
		};
		assert!(h265.in_band, "hev1 source should land as in_band=true");
		assert!(cfg.description.is_none(), "hev1 has no out-of-band description");
		assert_eq!(cfg.coded_width, Some(1280));
		assert_eq!(cfg.coded_height, Some(720));
	}
}
