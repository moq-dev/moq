use crate::codec::annexb::{NalIterator, START_CODE};
use crate::container::jitter::MinFrameDuration;

use bytes::{Buf, Bytes, BytesMut};
use scuffle_h265::{NALUnitType, SpsNALUnit};

use super::Error;
use crate::Result;

/// A decoder for H.265 with inline SPS/PPS.
/// Only supports single layer streams (VPS is cached but not parsed).
pub struct Import {
	// The broadcast being produced.
	broadcast: moq_net::BroadcastProducer,

	// The catalog being produced.
	catalog: crate::catalog::hang::Producer,

	// The track being produced.
	track: Option<crate::container::Producer<crate::catalog::hang::Container>>,

	// Whether the track has been initialized.
	// If it changes, then we'll reinitialize with a new track.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,

	// Cached parameter set NALs for re-insertion before keyframes.
	vps: Option<Bytes>,
	sps: Option<Bytes>,
	pps: Option<Bytes>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

impl Import {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::hang::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			current: Default::default(),
			zero: None,
			vps: None,
			sps: None,
			pps: None,
			jitter: MinFrameDuration::new(),
		}
	}

	fn init(&mut self, sps: &SpsNALUnit) -> Result<()> {
		let profile = &sps.rbsp.profile_tier_level.general_profile;
		let vui_data = sps.rbsp.vui_parameters.as_ref().map(VuiData::new).unwrap_or_default();

		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H265 {
			in_band: true, // We only support `hev1` with inline SPS/PPS for now
			profile_space: profile.profile_space,
			profile_idc: profile.profile_idc,
			profile_compatibility_flags: profile.profile_compatibility_flag.bits().to_be_bytes(),
			tier_flag: profile.tier_flag,
			level_idc: profile.level_idc.ok_or(Error::MissingLevelIdc)?,
			constraint_flags: crate::codec::h265::pack_constraint_flags(profile),
		});
		config.coded_width = Some(sps.rbsp.cropped_width() as u32);
		config.coded_height = Some(sps.rbsp.cropped_height() as u32);
		config.framerate = vui_data.framerate;
		config.display_ratio_width = vui_data.display_ratio_width;
		config.display_ratio_height = vui_data.display_ratio_height;
		config.container = hang::catalog::Container::Legacy;

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		let mut catalog = self.catalog.lock();

		if let Some(track) = &self.track.take() {
			tracing::debug!(name = ?track.name, "reinitializing track");
			catalog.video.renditions.remove(&track.name);
		}

		let track = self.broadcast.unique_track(".hev1")?;
		tracing::debug!(name = ?track.name, ?config, "starting track");
		catalog.video.renditions.insert(track.name.clone(), config.clone());

		self.config = Some(config);
		self.track = Some(crate::container::Producer::new(
			track,
			crate::catalog::hang::Container::Legacy,
		));

		Ok(())
	}

	/// Initialize the decoder with SPS/PPS and other non-slice NALs.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		let mut nals = NalIterator::new(buf);

		while let Some(nal) = nals.next().transpose()? {
			self.decode_nal(nal, None)?;
		}

		if let Some(nal) = nals.flush()? {
			self.decode_nal(nal, None)?;
		}

		Ok(())
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> Result<&moq_net::TrackProducer> {
		Ok(self.track.as_ref().ok_or(Error::NotInitialized)?.track())
	}

	/// Decode as much data as possible from the given buffer.
	///
	/// Unlike [Self::decode_frame], this method needs the start code for the next frame.
	/// This means it works for streaming media (ex. stdin) but adds a frame of latency.
	///
	/// TODO: This currently associates PTS with the *previous* frame, as part of `maybe_start_frame`.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
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
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
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
	fn decode_nal(&mut self, nal: Bytes, pts: Option<moq_net::Timestamp>) -> Result<()> {
		if nal.len() < 2 {
			return Err(Error::NalTooShort.into());
		}
		// u16 header: [forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)]
		let header = nal.first().ok_or(Error::NalTooShort)?;

		let forbidden_zero_bit = (header >> 7) & 1;
		if forbidden_zero_bit != 0 {
			return Err(Error::ForbiddenZeroBit.into());
		}

		// Bits 1-6: nal_unit_type
		let nal_unit_type = (header >> 1) & 0b111111;
		let nal_type = NALUnitType::from(nal_unit_type);

		match nal_type {
			NALUnitType::VpsNut => {
				self.maybe_start_frame(pts)?;

				self.vps = Some(nal.clone());
				self.current.contains_vps = true;
			}
			NALUnitType::SpsNut => {
				self.maybe_start_frame(pts)?;

				// Try to reinitialize the track if the SPS has changed.
				let sps = SpsNALUnit::parse(&mut &nal[..]).map_err(|_| Error::SpsParse)?;
				self.init(&sps)?;

				// SPS changed mid-AU. Cached VPS/PPS are tied to the old SPS
				// and may already have been appended to current.chunks earlier
				// in this AU; reset the AU so the new VPS+SPS+PPS triple is
				// the only parameter set we emit.
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
			// Keyframe containing slices
			NALUnitType::IdrWRadl
			| NALUnitType::IdrNLp
			| NALUnitType::BlaNLp
			| NALUnitType::BlaWRadl
			| NALUnitType::BlaWLp
			| NALUnitType::CraNut => {
				// Insert cached VPS/SPS/PPS before keyframes if not already present in this frame.
				if !self.current.contains_vps
					&& let Some(vps) = &self.vps
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(vps);
					self.current.contains_vps = true;
				}
				if !self.current.contains_sps
					&& let Some(sps) = &self.sps
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(sps);
					self.current.contains_sps = true;
				}
				if !self.current.contains_pps
					&& let Some(pps) = &self.pps
				{
					self.current.chunks.extend_from_slice(&START_CODE);
					self.current.chunks.extend_from_slice(pps);
					self.current.contains_pps = true;
				}

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
				if nal.get(2).ok_or(Error::NalTooShort)? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				self.current.contains_slice = true;
			}
			_ => {}
		}

		// Replace the original start code with a canonical 4-byte start code (marginally easier
		// for downstream players, e.g. MSE).
		self.current.chunks.extend_from_slice(&START_CODE);
		self.current.chunks.extend_from_slice(&nal);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<moq_net::Timestamp>) -> Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let track = self.track.as_mut().ok_or(Error::MissingSps)?;
		let pts = pts.ok_or(Error::MissingTimestamp)?;

		let payload = std::mem::take(&mut self.current.chunks).freeze();

		let frame = crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe: self.current.contains_idr,
		};

		track.write(frame)?;

		if let Some(jitter) = self.jitter.observe(pts)
			&& let Some(c) = self.catalog.lock().video.renditions.get_mut(&track.name)
		{
			c.jitter = Some(jitter);
		}

		self.current.contains_idr = false;
		self.current.contains_slice = false;
		self.current.contains_vps = false;
		self.current.contains_sps = false;
		self.current.contains_pps = false;

		Ok(())
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> Result<()> {
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;
		track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	///
	/// Any in-flight access unit is dropped. Pre-seek NALs would otherwise leak
	/// into the post-seek group with the wrong timestamp.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.current = Frame::default();
		let track = self.track.as_mut().ok_or(Error::NotInitialized)?;
		track.seek(sequence)?;
		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
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
		if let Some(track) = &self.track {
			tracing::debug!(name = ?track.name, "ending track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}
	}
}

#[derive(Default)]
struct Frame {
	chunks: BytesMut,
	contains_idr: bool,
	contains_slice: bool,
	contains_vps: bool,
	contains_sps: bool,
	contains_pps: bool,
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
