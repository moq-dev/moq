use crate as hang;
use crate::import::annexb::{NalIterator, START_CODE};

use anyhow::Context;
use buf_list::BufList;
use bytes::{Buf, Bytes};
use moq_lite as moq;
use scuffle_h265::{NALUnitType, SpsNALUnit};

/// A decoder for H.265 with inline SPS/PPS.
/// Only supports single layer streams, ignores VPS.
pub struct Hev1 {
	// The broadcast being produced.
	// This `hang` variant includes a catalog.
	broadcast: hang::BroadcastProducer,

	// The track being produced.
	track: Option<hang::TrackProducer>,

	// Whether the track has been initialized.
	// If it changes, then we'll reinitialize with a new track.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,
}

impl Hev1 {
	pub fn new(broadcast: hang::BroadcastProducer) -> Self {
		Self {
			broadcast,
			track: None,
			config: None,
			current: Default::default(),
			zero: None,
		}
	}

	fn init(&mut self, sps: &SpsNALUnit) -> anyhow::Result<()> {
		let profile = &sps.rbsp.profile_tier_level.general_profile;
		let vui_data = sps.rbsp.vui_parameters.as_ref().map(VuiData::new).unwrap_or_default();

		let config = hang::catalog::VideoConfig {
			coded_width: Some(sps.rbsp.cropped_width() as u32),
			coded_height: Some(sps.rbsp.cropped_height() as u32),
			codec: hang::catalog::H265 {
				in_band: true, // We only support `hev1` with inline SPS/PPS for now
				profile_space: profile.profile_space,
				profile_idc: profile.profile_idc,
				profile_compatibility_flags: profile.profile_compatibility_flag.bits().to_be_bytes(),
				tier_flag: profile.tier_flag,
				level_idc: profile.level_idc.context("missing level_idc in SPS")?,
				constraint_flags: pack_constraint_flags(profile),
			}
			.into(),
			description: None,
			framerate: vui_data.framerate,
			bitrate: None,
			display_ratio_width: vui_data.display_ratio_width,
			display_ratio_height: vui_data.display_ratio_height,
			optimize_for_latency: None,
			container: hang::catalog::Container::Legacy,
			min_buffer: None,
		};

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		if let Some(track) = &self.track.take() {
			tracing::debug!(name = ?track.info.name, "reinitializing track");
			self.broadcast.catalog.lock().remove_video(&track.info.name);
		}

		let track = moq::Track {
			name: self.broadcast.track_name("video"),
			priority: 2,
		};

		tracing::debug!(name = ?track.name, ?config, "starting track");

		{
			let mut catalog = self.broadcast.catalog.lock();
			let video = catalog.insert_video(track.name.clone(), config.clone());
			video.priority = 2;
		}

		let track = track.produce();
		self.broadcast.insert_track(track.consumer);

		self.config = Some(config);
		self.track = Some(track.producer.into());

		Ok(())
	}

	/// Initialize the decoder with SPS/PPS and other non-slice NALs.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		let mut nals = NalIterator::new(buf);

		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, None)?;
		}

		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, None)?;
		}

		Ok(())
	}

	/// Decode as much data as possible from the given buffer.
	///
	/// Unlike [Self::decode_frame], this method needs the start code for the next frame.
	/// This means it works for streaming media (ex. stdin) but adds a frame of latency.
	///
	/// TODO: This currently associates PTS with the *previous* frame, as part of `maybe_start_frame`.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::Timestamp>,
	) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;

		// Iterate over the NAL units in the buffer based on start codes.
		let nals = NalIterator::new(buf);

		for nal in nals {
			self.decode_nal(nal?, Some(pts))?;
		}

		Ok(())
	}

	/// Decode all data in the buffer, assuming the buffer contains (the rest of) a frame.
	///
	/// Unlike [Self::decode_stream], this is called when we know NAL boundaries.
	/// This can avoid a frame of latency just waiting for the next frame's start code.
	/// This can also be used when EOF is detected to flush the final frame.
	///
	/// NOTE: The next decode will fail if it doesn't begin with a start code.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::Timestamp>,
	) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;
		// Iterate over the NAL units in the buffer based on start codes.
		let mut nals = NalIterator::new(buf);

		// Iterate over each NAL that is followed by a start code.
		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, Some(pts))?;
		}

		// Assume the rest of the buffer is a single NAL.
		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, Some(pts))?;
		}

		// Flush the frame if we read a slice.
		self.maybe_start_frame(Some(pts))?;

		Ok(())
	}

	/// Decode a single NAL unit. Only reads the first header byte to extract nal_unit_type,
	/// Ignores nuh_layer_id and nuh_temporal_id_plus1.
	fn decode_nal(&mut self, nal: Bytes, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
		anyhow::ensure!(nal.len() >= 2, "NAL unit is too short");
		// u16 header: [forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)]
		let header = nal.first().context("NAL unit is too short")?;

		let forbidden_zero_bit = (header >> 7) & 1;
		anyhow::ensure!(forbidden_zero_bit == 0, "forbidden zero bit is not zero");

		// Bits 1-6: nal_unit_type
		let nal_unit_type = (header >> 1) & 0b111111;
		let nal_type = NALUnitType::from(nal_unit_type);

		match nal_type {
			NALUnitType::SpsNut => {
				self.maybe_start_frame(pts)?;

				// Try to reinitialize the track if the SPS has changed.
				let sps = SpsNALUnit::parse(&mut &nal[..]).context("failed to parse SPS NAL unit")?;
				self.init(&sps)?;
			}
			// TODO parse the SPS again and reinitialize the track if needed
			NALUnitType::AudNut | NALUnitType::PpsNut | NALUnitType::PrefixSeiNut | NALUnitType::SuffixSeiNut => {
				self.maybe_start_frame(pts)?;
			}
			// Keyframe containing slices
			NALUnitType::IdrWRadl
			| NALUnitType::IdrNLp
			| NALUnitType::BlaNLp
			| NALUnitType::BlaWRadl
			| NALUnitType::BlaWLp
			| NALUnitType::CraNut => {
				self.current.contains_idr = true;
				self.current.contains_slice = true;
			}
			// All other slice types (both N and R variants)
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
				// Check first_slice_segment_in_pic_flag (bit 7 of third byte, after 2-byte header)
				if nal.get(2).context("NAL unit is too short")? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				self.current.contains_slice = true;
			}
			_ => {}
		}

		// Rather than keeping the original size of the start code, we replace it with a 4 byte start code.
		// It's just marginally easier and potentially more efficient down the line (JS player with MSE).
		// NOTE: This is ref-counted and static, so it's extremely cheap to clone.
		self.current.chunks.push_chunk(START_CODE.clone());
		self.current.chunks.push_chunk(nal);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let track = self.track.as_mut().context("expected SPS before any frames")?;
		let pts = pts.context("missing timestamp")?;

		let payload = std::mem::take(&mut self.current.chunks);
		let frame = hang::Frame {
			timestamp: pts,
			keyframe: self.current.contains_idr,
			payload,
		};

		track.write(frame)?;

		self.current.contains_idr = false;
		self.current.contains_slice = false;

		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
	}

	fn pts(&mut self, hint: Option<hang::Timestamp>) -> anyhow::Result<hang::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(hang::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

impl Drop for Hev1 {
	fn drop(&mut self) {
		if let Some(track) = &self.track {
			tracing::debug!(name = ?track.info.name, "ending track");
			self.broadcast.catalog.lock().remove_video(&track.info.name);
		}
	}
}

// Packs the constraint flags from ITU H.265 V10 Section 7.3.3 Profile, tier and level syntax
fn pack_constraint_flags(profile: &scuffle_h265::Profile) -> [u8; 6] {
	let mut flags = [0u8; 6];
	flags[0] = ((profile.progressive_source_flag as u8) << 7)
		| ((profile.interlaced_source_flag as u8) << 6)
		| ((profile.non_packed_constraint_flag as u8) << 5)
		| ((profile.frame_only_constraint_flag as u8) << 4);

	// @todo: pack the rest of the optional flags in profile.additional_flags
	flags
}

#[derive(Default)]
struct Frame {
	chunks: BufList,
	contains_idr: bool,
	contains_slice: bool,
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
