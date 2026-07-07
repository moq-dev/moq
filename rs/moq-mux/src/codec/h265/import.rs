//! H.265 importer.
//!
//! Publishes H.265 frames (Annex-B, inline VPS/SPS/PPS, the "hev1" shape) on a
//! single moq track and resolves the catalog rendition. Only single-layer
//! streams are supported (VPS is cached but not parsed).
//!
//! The codec config is scanned out of the SPS the splitter packages into the
//! first keyframe (or seeded via [`initialize`](Import::initialize)). A keyframe
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
use crate::container::jitter::Jitter;

/// A pure-publisher importer for H.265 with inline VPS/SPS/PPS.
/// Only supports single layer streams (VPS is cached but not parsed).
///
/// Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from, and feed it
/// frames a [`Split`](super::Split) produced via [`decode`](Self::decode). The
/// catalog rendition fills in lazily once the first SPS is parsed.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::VideoTrack<E>,
	config: Option<hang::catalog::VideoConfig>,
	last_sps: Option<Bytes>,
	jitter: Jitter,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, reserving the rendition from `reserved`.
	pub fn new(track: moq_net::track::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		let rendition = reserved.video(track.name());
		Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
			config: None,
			last_sps: None,
			jitter: Jitter::new(),
		}
	}

	/// Resolve the codec config from VPS/SPS/PPS and other non-slice NALs.
	///
	/// Resolves the config from any SPS in the buffer. Optional, since the importer
	/// also self-initializes from the first keyframe. Takes a read-only slice: the
	/// dispatcher-owned [`Split`](super::Split) is what consumes the stream (and seeds
	/// its parameter-set cache).
	pub fn initialize(&mut self, buf: &[u8]) -> Result<()> {
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
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Record a frame's reorder delay (`PTS - DTS`) so the catalog `jitter` reflects the
	/// B-frame reorder depth (the decode buffer a transmuxer/player must hold). The
	/// container supplies this since the elementary stream alone carries no decode time.
	pub fn observe_reorder(&mut self, reorder: moq_net::Timestamp) {
		if let Some(jitter) = self.jitter.observe_reorder(reorder) {
			self.rendition.update(|c| c.jitter = Some(jitter));
		}
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
			in_band: true, // We only support `hev1` with inline VPS/SPS/PPS for now.
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

		// A changed SPS just re-mirrors the rendition in place; there are no fixed
		// tracks to reject a reconfiguration.
		if self.config.as_ref() == Some(&config) {
			return Ok(());
		}

		tracing::debug!(name = ?self.track.name(), ?config, "starting track");
		self.rendition.set(config.clone());
		// Seed jitter from whatever has accumulated: a dirty start (or a B-frame
		// reorder observed via observe_reorder) can feed updates before this
		// rendition exists, so those would otherwise be lost on (re)publish.
		if let Some(jitter) = self.jitter.current() {
			self.rendition.update(|c| c.jitter = Some(jitter));
		}
		self.config = Some(config);
		Ok(())
	}

	/// Write split frames to the track, resolving the config from the first
	/// keyframe's inline SPS and refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			if frame.keyframe
				&& let Some(sps) = find_sps(&frame.payload)
			{
				self.configure_from_sps(&sps)?;
			}

			// A keyframe we still can't configure (no SPS) is undecodable.
			if frame.keyframe && self.config.is_none() {
				return Err(Error::MissingSps.into());
			}

			let pts = frame.timestamp;
			// A pre-keyframe delta has no group to anchor it: the producer returns
			// MissingKeyframe, which the caller (e.g. a TS mid-stream join) skips.
			self.track.write(frame)?;

			if let Some(jitter) = self.jitter.observe(pts) {
				self.rendition.update(|c| c.jitter = Some(jitter));
			}
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
