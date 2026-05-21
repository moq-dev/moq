use super::annexb::NalIterator;
use super::jitter::MinFrameDuration;
use super::same_codec;

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use scuffle_h265::{NALUnitType, SpsNALUnit};

/// A decoder for H.265 with inline SPS/PPS.
/// Only supports single layer streams (VPS is cached but not parsed).
pub struct Hev1 {
	// The broadcast being produced.
	broadcast: moq_net::BroadcastProducer,

	// The catalog being produced.
	catalog: crate::catalog::Producer,

	// The track being produced.
	track: Option<crate::container::Producer<crate::container::Hang>>,

	// Whether the track has been initialized.
	// If it changes, then we'll reinitialize with a new track.
	config: Option<hang::catalog::VideoConfig>,

	// The current frame being built.
	current: Frame,

	// Used to compute wall clock timestamps if needed.
	zero: Option<tokio::time::Instant>,

	// Cached parameter set NALs for re-insertion before keyframes.
	cached_vps: Option<Bytes>,
	cached_sps: Option<Bytes>,
	cached_pps: Option<Bytes>,

	// Tracks the minimum frame duration and updates the catalog `jitter` field.
	jitter: MinFrameDuration,
}

impl Hev1 {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			config: None,
			current: Default::default(),
			zero: None,
			cached_vps: None,
			cached_sps: None,
			cached_pps: None,
			jitter: MinFrameDuration::new(),
		}
	}

	fn init(&mut self, sps: &SpsNALUnit) -> anyhow::Result<()> {
		let profile = &sps.rbsp.profile_tier_level.general_profile;
		let vui_data = sps.rbsp.vui_parameters.as_ref().map(VuiData::new).unwrap_or_default();

		// hvcC is emitted once all of VPS, SPS, and PPS have been observed.
		let description = match (&self.cached_vps, &self.cached_sps, &self.cached_pps) {
			(Some(vps_nal), Some(sps_nal), Some(pps_nal)) => Some(build_hvcc(vps_nal, sps_nal, pps_nal)?),
			_ => None,
		};

		let config = hang::catalog::VideoConfig {
			coded_width: Some(sps.rbsp.cropped_width() as u32),
			coded_height: Some(sps.rbsp.cropped_height() as u32),
			codec: hang::catalog::H265 {
				// VPS/SPS/PPS now live in `description` (hvcC) and are
				// stripped from sample data — that's the hvc1 contract.
				in_band: false,
				profile_space: profile.profile_space,
				profile_idc: profile.profile_idc,
				profile_compatibility_flags: profile.profile_compatibility_flag.bits().to_be_bytes(),
				tier_flag: profile.tier_flag,
				level_idc: profile.level_idc.context("missing level_idc in SPS")?,
				constraint_flags: pack_constraint_flags(profile),
			}
			.into(),
			description,
			framerate: vui_data.framerate,
			bitrate: None,
			display_ratio_width: vui_data.display_ratio_width,
			display_ratio_height: vui_data.display_ratio_height,
			optimize_for_latency: None,
			container: hang::catalog::Container::Legacy,
			jitter: None,
		};

		if let Some(old) = &self.config
			&& old == &config
		{
			return Ok(());
		}

		// Codec-bearing fields determine track identity. A pure description
		// update (e.g. cached_pps just arrived) reuses the existing track.
		let needs_retrack = self.track.is_none() || !self.config.as_ref().is_some_and(|old| same_codec(old, &config));

		// Mint the replacement track BEFORE touching the catalog. If
		// unique_track fails we leave self.track and the catalog untouched.
		let new_producer = if needs_retrack {
			Some(crate::container::Producer::new(
				self.broadcast.unique_track(".hev1")?,
				crate::container::Hang::Legacy,
			))
		} else {
			None
		};

		let mut catalog = self.catalog.lock();

		if let Some(new) = new_producer {
			if let Some(old) = self.track.as_ref() {
				tracing::debug!(old_name = ?old.name, new_name = ?new.name, "codec changed; replacing track");
				catalog.video.renditions.remove(&old.name);
			} else {
				tracing::debug!(name = ?new.name, ?config, "starting track");
			}
			catalog.video.renditions.insert(new.name.clone(), config.clone());
			self.track = Some(new);
		} else if let Some(track) = self.track.as_ref() {
			tracing::debug!(name = ?track.name, "updating rendition (description)");
			catalog.video.renditions.insert(track.name.clone(), config.clone());
		}

		self.config = Some(config);

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

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> anyhow::Result<&moq_net::TrackProducer> {
		Ok(&self.track.as_ref().context("not initialized")?.track)
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
		pts: Option<hang::container::Timestamp>,
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
		pts: Option<hang::container::Timestamp>,
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
	fn decode_nal(&mut self, nal: Bytes, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		anyhow::ensure!(nal.len() >= 2, "NAL unit is too short");
		// u16 header: [forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)]
		let header = nal.first().context("NAL unit is too short")?;

		let forbidden_zero_bit = (header >> 7) & 1;
		anyhow::ensure!(forbidden_zero_bit == 0, "forbidden zero bit is not zero");

		// Bits 1-6: nal_unit_type
		let nal_unit_type = (header >> 1) & 0b111111;
		let nal_type = NALUnitType::from(nal_unit_type);

		// VPS/SPS/PPS go into the catalog `description` (hvcC) and are stripped
		// from sample data — that's the hvc1 contract. Everything else is
		// emitted length-prefixed (4-byte big-endian, matching the
		// `lengthSizeMinusOne = 3` we write in build_hvcc).
		let emit = match nal_type {
			NALUnitType::VpsNut => {
				self.maybe_start_frame(pts)?;
				self.cached_vps = Some(nal.clone());

				// If SPS was already cached, republish so hvcC picks up the new VPS.
				if let Some(sps_nal) = self.cached_sps.clone()
					&& let Ok(sps) = SpsNALUnit::parse(&mut &sps_nal[..])
				{
					self.init(&sps)?;
				}
				false
			}
			NALUnitType::SpsNut => {
				self.maybe_start_frame(pts)?;

				let sps = SpsNALUnit::parse(&mut &nal[..]).context("failed to parse SPS NAL unit")?;

				// PPS is tied to SPS context; drop cached PPS when SPS changes.
				if self.cached_sps.as_ref().is_some_and(|cached| cached != &nal) {
					self.cached_pps = None;
				}

				// Cache before init() so the hvcC builder can see the latest SPS.
				self.cached_sps = Some(nal.clone());

				self.init(&sps)?;
				false
			}
			NALUnitType::PpsNut => {
				self.maybe_start_frame(pts)?;
				self.cached_pps = Some(nal.clone());

				// First PPS after VPS+SPS unlocks hvcC emission — republish the catalog.
				if let Some(sps_nal) = self.cached_sps.clone()
					&& let Ok(sps) = SpsNALUnit::parse(&mut &sps_nal[..])
				{
					self.init(&sps)?;
				}
				false
			}
			NALUnitType::AudNut | NALUnitType::PrefixSeiNut | NALUnitType::SuffixSeiNut => {
				self.maybe_start_frame(pts)?;
				true
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
				true
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
				true
			}
			_ => true,
		};

		if emit {
			let len = u32::try_from(nal.len()).context("NAL too large for 4-byte length prefix")?;
			self.current.chunks.extend_from_slice(&len.to_be_bytes());
			self.current.chunks.extend_from_slice(&nal);
		}

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		// If we haven't seen any slices, we shouldn't flush yet.
		if !self.current.contains_slice {
			return Ok(());
		}

		let track = self.track.as_mut().context("expected SPS before any frames")?;
		let pts = pts.context("missing timestamp")?;

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
	pub fn finish(&mut self) -> anyhow::Result<()> {
		let track = self.track.as_mut().context("not initialized")?;
		track.finish()?;
		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
	}

	fn pts(&mut self, hint: Option<hang::container::Timestamp>) -> anyhow::Result<hang::container::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(hang::container::Timestamp::from_micros(
			zero.elapsed().as_micros() as u64
		)?)
	}
}

impl Drop for Hev1 {
	fn drop(&mut self) {
		if let Some(track) = &self.track {
			tracing::debug!(name = ?track.name, "ending track");
			self.catalog.lock().video.renditions.remove(&track.name);
		}
	}
}

/// Build an HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3) from a
/// single VPS, SPS, and PPS NAL. Single-layer streams only — multi-layer
/// VPS extensions are not represented.
///
/// Errors if any NAL is too large to fit hvcC's 16-bit length fields, or if
/// the SPS cannot be parsed.
fn build_hvcc(vps_nal: &[u8], sps_nal: &[u8], pps_nal: &[u8]) -> anyhow::Result<Bytes> {
	use bytes::BufMut;

	for (label, nal) in [("VPS", vps_nal), ("SPS", sps_nal), ("PPS", pps_nal)] {
		anyhow::ensure!(
			nal.len() <= u16::MAX as usize,
			"{} too large for hvcC length field ({} > {})",
			label,
			nal.len(),
			u16::MAX
		);
	}

	let sps = SpsNALUnit::parse(&mut &sps_nal[..]).context("failed to parse SPS NAL unit for hvcC")?;
	let profile = &sps.rbsp.profile_tier_level.general_profile;
	let level_idc = profile.level_idc.context("missing level_idc in SPS")?;
	let constraint_flags = pack_constraint_flags(profile);
	let compat = profile.profile_compatibility_flag.bits().to_be_bytes();
	let num_temporal_layers = sps.rbsp.sps_max_sub_layers_minus1 + 1;

	let mut out = BytesMut::with_capacity(23 + vps_nal.len() + sps_nal.len() + pps_nal.len() + 9 * 3);
	out.put_u8(1); // configurationVersion
	// general_profile_space(2) | general_tier_flag(1) | general_profile_idc(5)
	out.put_u8(((profile.profile_space & 0x3) << 6) | ((profile.tier_flag as u8) << 5) | (profile.profile_idc & 0x1f));
	out.put_slice(&compat); // general_profile_compatibility_flags (32 bits)
	out.put_slice(&constraint_flags); // general_constraint_indicator_flags (48 bits)
	out.put_u8(level_idc); // general_level_idc
	// reserved(4) | min_spatial_segmentation_idc(12) — 0 means "unknown"
	out.put_u16(0xf000);
	out.put_u8(0xfc); // reserved(6) | parallelismType(2) — 0 = mixed
	out.put_u8(0xfc | (sps.rbsp.chroma_format_idc & 0x3)); // reserved(6) | chromaFormat(2)
	out.put_u8(0xf8 | (sps.rbsp.bit_depth_luma_minus8 & 0x7)); // reserved(5) | bitDepthLumaMinus8(3)
	out.put_u8(0xf8 | (sps.rbsp.bit_depth_chroma_minus8 & 0x7)); // reserved(5) | bitDepthChromaMinus8(3)
	out.put_u16(0); // avgFrameRate — unspecified
	// constantFrameRate(2) | numTemporalLayers(3) | temporalIdNested(1) | lengthSizeMinusOne(2, =3)
	out.put_u8(((num_temporal_layers & 0x7) << 3) | ((sps.rbsp.sps_temporal_id_nesting_flag as u8) << 2) | 0x3);
	out.put_u8(3); // numOfArrays — VPS, SPS, PPS

	// Each array: array_completeness(1) | reserved(1) | NAL_unit_type(6) | numNalus(16) | (nalUnitLength(16) | nalUnit)*
	let nal_unit_type_vps = u8::from(scuffle_h265::NALUnitType::VpsNut);
	let nal_unit_type_sps = u8::from(scuffle_h265::NALUnitType::SpsNut);
	let nal_unit_type_pps = u8::from(scuffle_h265::NALUnitType::PpsNut);

	for (nal_type, nal) in [(nal_unit_type_vps, vps_nal), (nal_unit_type_sps, sps_nal), (nal_unit_type_pps, pps_nal)] {
		out.put_u8(0x80 | (nal_type & 0x3f)); // array_completeness = 1
		out.put_u16(1); // numNalus
		out.put_u16(nal.len() as u16);
		out.put_slice(nal);
	}

	Ok(out.freeze())
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

#[cfg(test)]
mod tests {
	use super::*;

	// Overflow checks run before SPS parsing, so the SPS bytes only need to
	// be present and short — they're never actually parsed in these tests.
	#[test]
	fn hvcc_errors_on_oversized_vps() {
		let vps = vec![0u8; u16::MAX as usize + 1];
		let sps = vec![0x42, 0x01];
		let pps = vec![0x44, 0x01];
		let err = build_hvcc(&vps, &sps, &pps).expect_err("oversized VPS should error");
		assert!(err.to_string().contains("VPS too large"), "got: {err}");
	}

	#[test]
	fn hvcc_errors_on_oversized_sps() {
		let vps = vec![0x40, 0x01];
		let sps = vec![0u8; u16::MAX as usize + 1];
		let pps = vec![0x44, 0x01];
		let err = build_hvcc(&vps, &sps, &pps).expect_err("oversized SPS should error");
		assert!(err.to_string().contains("SPS too large"), "got: {err}");
	}

	#[test]
	fn hvcc_errors_on_oversized_pps() {
		let vps = vec![0x40, 0x01];
		let sps = vec![0x42, 0x01];
		let pps = vec![0u8; u16::MAX as usize + 1];
		let err = build_hvcc(&vps, &sps, &pps).expect_err("oversized PPS should error");
		assert!(err.to_string().contains("PPS too large"), "got: {err}");
	}
}
