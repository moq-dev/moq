//! The H.265 half of the encoder: Main profile parameter sets and slice headers
//! written here, and the VA-API buffers that describe each picture.
//!
//! The parameter sets and slice headers are written here and handed to the
//! driver packed, so it produces only the slice data. That is what ffmpeg's
//! `hevc_vaapi` does, and the path the Intel and AMD drivers are exercised on.
//! A driver must take the packed parameter sets, since VA's sequence parameters
//! cannot carry the conformance window or the VUI; one that does not take
//! packed slice headers writes them itself, from parameter buffers that
//! describe the same stream.
//!
//! The coding tools follow what the driver reports through
//! `VAConfigAttribEncHEVCFeatures` and `VAConfigAttribEncHEVCBlockSizes`: a
//! tool it supports or requires is on, and one it requires that this writer
//! does not emit fails the open. A driver that can predict only from
//! non-empty lists in both directions (`VA_PREDICTION_DIRECTION_BI_NOT_EMPTY`,
//! Intel's low-power encoder among them) gets each P picture as a generalized
//! B picture: a B slice whose list 1 repeats list 0.

use std::rc::Rc;

use anyhow::{anyhow, bail};

use super::{Config, Reference};
use crate::{
	bindings, BufferType, Config as VaConfig, Display, EncCodedBuffer, EncPackedHeaderType, EncPictureParameter,
	EncPictureParameterBufferHEVC, EncSequenceParameter, EncSequenceParameterBufferHEVC, EncSliceParameter,
	EncSliceParameterBufferHEVC, GenericValue, HEVCEncPicFields, HEVCEncSeqFields, HevcEncPicSccFields,
	HevcEncSeqSccFields, HevcEncSliceFields, PictureHEVC, VAConfigAttrib, VAConfigAttribType, VAEntrypoint, VAProfile,
	VASurfaceAttribType, VA_ATTRIB_NOT_SUPPORTED, VA_ENC_PACKED_HEADER_SEQUENCE, VA_ENC_PACKED_HEADER_SLICE,
	VA_INVALID_ID, VA_PICTURE_HEVC_INVALID, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE, VA_PREDICTION_DIRECTION_BI_NOT_EMPTY,
};

/// NAL unit types, H.265 Table 7-1.
const TRAIL_R: u8 = 1;
const IDR_W_RADL: u8 = 19;
const VPS: u8 = 32;
const SPS: u8 = 33;
const PPS: u8 = 34;

/// Slice types, H.265 Table 7-7.
const SLICE_B: u8 = 0;
const SLICE_P: u8 = 1;
const SLICE_I: u8 = 2;

/// `general_profile_idc` of the Main profile.
const MAIN: u8 = 1;

/// `log2_max_pic_order_cnt_lsb_minus4`: twelve bits of order count per slice, as ffmpeg writes.
const LOG2_MAX_POC_LSB_MINUS4: u32 = 8;

/// The QP the PPS opens each picture at; rate control moves off it per picture.
const INIT_QP: i32 = 26;

/// The VA profile this encoder opens.
pub(super) const PROFILE: VAProfile::Type = VAProfile::VAProfileHEVCMain;

/// How a feature of `VAConfigAttribValEncHEVCFeatures` is offered.
const UNSUPPORTED: u32 = 0;
const REQUIRED: u32 = 2;

/// The coding tools and block sizes a stream is written with, settled once from what the driver reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tools {
	log2_min_cb: u32,
	log2_ctb: u32,
	log2_min_tb: u32,
	log2_max_tb: u32,
	depth_inter: u32,
	depth_intra: u32,
	amp: bool,
	sao: bool,
	temporal_mvp: bool,
	transform_skip: bool,
	cu_qp_delta: bool,
	/// Whether P pictures go out as B slices whose list 1 repeats list 0.
	gpb: bool,
}

impl Tools {
	/// What ffmpeg assumes of a driver too old to report its block sizes and features: the first Intel HEVC encoder's.
	const FALLBACK: Self = Self {
		log2_min_cb: 4,
		log2_ctb: 5,
		log2_min_tb: 2,
		log2_max_tb: 5,
		depth_inter: 3,
		depth_intra: 3,
		amp: true,
		sao: false,
		temporal_mvp: false,
		transform_skip: false,
		cu_qp_delta: true,
		gpb: false,
	};

	/// Asks `display` what its H.265 Main encoder on `entrypoint` supports.
	///
	/// # Errors
	///
	/// Fails when the driver cannot be asked, when it predicts from no
	/// previous picture, and when it requires a tool this writer does not emit.
	fn query(display: &Display, entrypoint: VAEntrypoint::Type) -> anyhow::Result<Self> {
		let mut attrs = [
			VAConfigAttribType::VAConfigAttribEncHEVCFeatures,
			VAConfigAttribType::VAConfigAttribEncHEVCBlockSizes,
			VAConfigAttribType::VAConfigAttribEncMaxRefFrames,
			VAConfigAttribType::VAConfigAttribPredictionDirection,
		]
		.map(|type_| VAConfigAttrib { type_, value: 0 });
		display
			.get_config_attributes(PROFILE, entrypoint, &mut attrs)
			.map_err(|e| anyhow!("query H.265 encode attributes: {e:?}"))?;
		let reported = |index: usize| Some(attrs[index].value).filter(|&value| value != VA_ATTRIB_NOT_SUPPORTED);
		let (features, blocks, references, direction) = (reported(0), reported(1), reported(2), reported(3));

		let mut tools = Self::FALLBACK;
		if let Some(blocks) = blocks {
			let blocks = bindings::VAConfigAttribValEncHEVCBlockSizes { value: blocks };
			// Safe because both members of the union are plain integers over the same 32 bits.
			let blocks = unsafe { blocks.bits };
			tools.log2_ctb = blocks.log2_max_coding_tree_block_size_minus3() + 3;
			tools.log2_min_cb = blocks.log2_min_luma_coding_block_size_minus3() + 3;
			tools.log2_min_tb = blocks.log2_min_luma_transform_block_size_minus2() + 2;
			tools.log2_max_tb = blocks.log2_max_luma_transform_block_size_minus2() + 2;
			tools.depth_inter = blocks.max_max_transform_hierarchy_depth_inter();
			tools.depth_intra = blocks.max_max_transform_hierarchy_depth_intra();
		}
		if let Some(features) = features {
			let features = bindings::VAConfigAttribValEncHEVCFeatures { value: features };
			// Safe because both members of the union are plain integers over the same 32 bits.
			let features = unsafe { features.bits };
			tools.amp = features.amp() != UNSUPPORTED;
			tools.sao = features.sao() != UNSUPPORTED;
			tools.temporal_mvp = features.temporal_mvp() != UNSUPPORTED;
			tools.transform_skip = features.transform_skip() != UNSUPPORTED;
			tools.cu_qp_delta = features.cu_qp_delta() != UNSUPPORTED;
			for (name, offered) in [
				("separate colour planes", features.separate_colour_planes()),
				("scaling lists", features.scaling_lists()),
				("PCM", features.pcm()),
				("strong intra smoothing", features.strong_intra_smoothing()),
				("dependent slices", features.dependent_slices()),
				("sign data hiding", features.sign_data_hiding()),
				("constrained intra prediction", features.constrained_intra_pred()),
				("weighted prediction", features.weighted_prediction()),
				("transquant bypass", features.transquant_bypass()),
				("a disabled deblocking filter", features.deblocking_filter_disable()),
			] {
				if offered == REQUIRED {
					bail!("the driver requires {name}, which this encoder does not write");
				}
			}
		}
		if tools.log2_min_cb > tools.log2_ctb
			|| tools.log2_min_tb > tools.log2_max_tb
			|| tools.log2_max_tb > tools.log2_ctb.min(5)
			|| tools.log2_min_tb >= tools.log2_min_cb
		{
			bail!("the driver reports inconsistent H.265 block sizes: {tools:?}");
		}

		let (list0, list1) = match references {
			Some(value) => (value & 0xffff, value >> 16 & 0xffff),
			None => (1, 0),
		};
		if list0 == 0 {
			bail!("the driver's H.265 encoder predicts from no reference picture");
		}
		tools.gpb =
			list1 > 0 && direction.is_some_and(|direction| direction & VA_PREDICTION_DIRECTION_BI_NOT_EMPTY != 0);
		Ok(tools)
	}
}

/// An H.265 stream's settled parameters: everything per picture derives from these.
pub(super) struct Stream {
	tools: Tools,
	/// The coded size: the frame rounded up to whole minimum coding blocks and to the driver's surface alignment.
	coded: (u32, u32),
	level_idc: u8,
	/// The VPS, SPS, and PPS, each behind a start code, sent ahead of every IDR.
	headers: Vec<u8>,
}

impl Stream {
	/// Settles the parameters for `config` on `display`'s encoder at `entrypoint`, configured as `va_config`.
	///
	/// # Errors
	///
	/// Fails as [`Tools::query`] does, and for a frame too large for any level.
	pub(super) fn new(
		display: &Display,
		entrypoint: VAEntrypoint::Type,
		va_config: &mut VaConfig,
		config: &Config,
	) -> anyhow::Result<Self> {
		let tools = Tools::query(display, entrypoint)?;
		Self::with_tools(tools, alignment(va_config), config)
	}

	/// `alignment` is the driver's surface alignment, in samples across and down.
	fn with_tools(tools: Tools, alignment: (u32, u32), config: &Config) -> anyhow::Result<Self> {
		let min_cb = 1 << tools.log2_min_cb;
		let coded = (
			config.width.next_multiple_of(min_cb.max(alignment.0)),
			config.height.next_multiple_of(min_cb.max(alignment.1)),
		);
		let level_idc = level(coded, config.framerate).ok_or_else(|| {
			anyhow!(
				"{}x{} at {} fps exceeds every H.265 level",
				coded.0,
				coded.1,
				config.framerate
			)
		})?;
		let mut stream = Self {
			tools,
			coded,
			level_idc,
			headers: Vec::new(),
		};
		stream.headers = [stream.vps(config), stream.sps(config), stream.pps()].concat();
		Ok(stream)
	}

	/// Returns the coded size: what the context, the reconstructed surfaces, and the SPS are sized to.
	pub(super) fn coded(&self) -> (u32, u32) {
		self.coded
	}

	/// Returns the VA buffers for one picture, in the order the driver takes them: sequence, picture, slice, then packed headers.
	pub(super) fn buffers(&self, config: &Config, picture: &super::PictureInfo<'_>, packed: u32) -> Vec<BufferType> {
		let tools = &self.tools;
		let poc = picture.count as i32;
		let current = PictureHEVC::new(picture.recon, poc, 0);
		let reference = picture.reference.map(|Reference { surface, count }| {
			PictureHEVC::new(surface, count as i32, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE)
		});
		let slice_type = match (reference.is_some(), tools.gpb) {
			(false, _) => SLICE_I,
			(true, false) => SLICE_P,
			(true, true) => SLICE_B,
		};
		let nal_type = if picture.idr { IDR_W_RADL } else { TRAIL_R };

		let mut buffers = vec![
			self.sequence_parameters(config, picture.bitrate),
			self.picture_parameters(picture.coded, current, reference.as_ref(), nal_type),
			self.slice_parameters(slice_type, reference.as_ref()),
		];
		if picture.idr && packed & VA_ENC_PACKED_HEADER_SEQUENCE != 0 {
			buffers.extend(super::packed_header(
				EncPackedHeaderType::Sequence,
				&self.headers,
				self.headers.len() * 8,
			));
		}
		if packed & VA_ENC_PACKED_HEADER_SLICE != 0 {
			let header = self.slice_header(nal_type, slice_type, picture.count);
			buffers.extend(super::packed_header(
				EncPackedHeaderType::Slice,
				&header,
				header.len() * 8,
			));
		}
		buffers
	}

	fn ctb_count(&self) -> u32 {
		let ctb = 1 << self.tools.log2_ctb;
		self.coded.0.div_ceil(ctb) * self.coded.1.div_ceil(ctb)
	}

	fn sequence_parameters(&self, config: &Config, bitrate: u32) -> BufferType {
		let tools = &self.tools;
		let fields = HEVCEncSeqFields::new(
			1, // chroma_format_idc: 4:2:0
			0,
			0,
			0,
			0,
			0,
			tools.amp as u32,
			tools.sao as u32,
			0,
			0,
			tools.temporal_mvp as u32,
			0,
			0,
		);
		BufferType::EncSequenceParameter(EncSequenceParameter::HEVC(EncSequenceParameterBufferHEVC::new(
			MAIN,
			self.level_idc,
			0, // Main tier
			config.gop,
			config.gop,
			1, // ip_period: no B pictures
			bitrate,
			self.coded.0 as u16,
			self.coded.1 as u16,
			&fields,
			(tools.log2_min_cb - 3) as u8,
			(tools.log2_ctb - tools.log2_min_cb) as u8,
			(tools.log2_min_tb - 2) as u8,
			(tools.log2_max_tb - tools.log2_min_tb) as u8,
			tools.depth_inter as u8,
			tools.depth_intra as u8,
			0,
			0,
			0,
			0,
			// The VUI is in the packed SPS, which is what goes out.
			None,
			0,
			0,
			0,
			0,
			0,
			0,
			0,
			0,
			&HevcEncSeqSccFields::new(0),
		)))
	}

	fn picture_parameters(
		&self,
		coded: &EncCodedBuffer,
		current: PictureHEVC,
		reference: Option<&PictureHEVC>,
		nal_type: u8,
	) -> BufferType {
		let tools = &self.tools;
		let idr = nal_type == IDR_W_RADL;
		let fields = HEVCEncPicFields::new(
			idr as u32,
			if reference.is_some() { 2 } else { 1 }, // coding_type: P (or GPB), else I
			1,                                       // reference_pic_flag
			0,
			0,
			0,
			tools.transform_skip as u32,
			tools.cu_qp_delta as u32,
			0,
			0,
			0,
			0,
			0,
			0,
			1, // pps_loop_filter_across_slices_enabled_flag
			0,
			0,
			0,
			0,
		);
		let mut references: [PictureHEVC; 15] = std::array::from_fn(|_| invalid());
		if let Some(reference) = reference {
			references[0] = reference.clone();
		}
		BufferType::EncPictureParameter(EncPictureParameter::HEVC(EncPictureParameterBufferHEVC::new(
			current,
			references,
			coded.id(),
			if tools.temporal_mvp { 0 } else { 0xff },
			0,
			INIT_QP as u8,
			self.diff_cu_qp_delta_depth() as u8,
			0,
			0,
			0,
			0,
			[0; 19],
			[0; 21],
			0,
			0,
			0,
			0,
			0,
			nal_type,
			&fields,
			0,
			0,
			&HevcEncPicSccFields::new(0),
		)))
	}

	fn slice_parameters(&self, slice_type: u8, reference: Option<&PictureHEVC>) -> BufferType {
		let tools = &self.tools;
		let mut list0: [PictureHEVC; 15] = std::array::from_fn(|_| invalid());
		let mut list1: [PictureHEVC; 15] = std::array::from_fn(|_| invalid());
		if let Some(reference) = reference {
			list0[0] = reference.clone();
			if slice_type == SLICE_B {
				list1[0] = reference.clone();
			}
		}
		let fields = HevcEncSliceFields::new(
			1, // last_slice_of_pic_flag
			0,
			0,
			(tools.temporal_mvp && reference.is_some()) as u32,
			tools.sao as u32,
			tools.sao as u32,
			0,
			0,
			0,
			0,
			0,
			1, // collocated_from_l0_flag
		);
		BufferType::EncSliceParameter(EncSliceParameter::HEVC(EncSliceParameterBufferHEVC::new(
			0,
			self.ctb_count(),
			slice_type,
			0,
			0,
			0,
			list0,
			list1,
			0,
			0,
			[0; 15],
			[0; 15],
			[[0; 2]; 15],
			[[0; 2]; 15],
			[0; 15],
			[0; 15],
			[[0; 2]; 15],
			[[0; 2]; 15],
			5, // max_num_merge_cand
			0,
			0,
			0,
			0,
			0,
			&fields,
			0,
			0,
		)))
	}

	fn diff_cu_qp_delta_depth(&self) -> u32 {
		match self.tools.cu_qp_delta {
			true => self.tools.log2_ctb - self.tools.log2_min_cb,
			false => 0,
		}
	}

	/// Writes `profile_tier_level` for Main with no sub-layers, H.265 7.3.3.
	fn profile_tier_level(&self, w: &mut Rbsp) {
		w.bits(2, 0); // general_profile_space
		w.flag(false); // general_tier_flag: Main
		w.bits(5, MAIN.into());
		// Main, and Main 10 which every Main stream also conforms to.
		w.bits(32, 1 << (31 - 1) | 1 << (31 - 2));
		w.flag(true); // general_progressive_source_flag
		w.flag(false); // general_interlaced_source_flag
		w.flag(true); // general_non_packed_constraint_flag
		w.flag(true); // general_frame_only_constraint_flag
				// The 43 reserved bits and general_inbld_flag of a Main profile.
		w.bits(32, 0);
		w.bits(12, 0);
		w.bits(8, self.level_idc.into());
	}

	/// Writes the video parameter set, H.265 7.3.2.1.
	fn vps(&self, config: &Config) -> Vec<u8> {
		let mut w = Rbsp::default();
		w.bits(4, 0); // vps_video_parameter_set_id
		w.flag(true); // vps_base_layer_internal_flag
		w.flag(true); // vps_base_layer_available_flag
		w.bits(6, 0); // vps_max_layers_minus1
		w.bits(3, 0); // vps_max_sub_layers_minus1
		w.flag(true); // vps_temporal_id_nesting_flag
		w.bits(16, 0xffff);
		self.profile_tier_level(&mut w);
		w.flag(false); // vps_sub_layer_ordering_info_present_flag
		ordering(&mut w);
		w.bits(6, 0); // vps_max_layer_id
		w.ue(0); // vps_num_layer_sets_minus1
		w.flag(true); // vps_timing_info_present_flag
		timing(&mut w, config.framerate);
		w.ue(0); // vps_num_hrd_parameters
		w.flag(false); // vps_extension_flag
		w.trailing();
		nal(VPS, &w.finish())
	}

	/// Writes the sequence parameter set, H.265 7.3.2.2, with its VUI.
	fn sps(&self, config: &Config) -> Vec<u8> {
		let tools = &self.tools;
		let mut w = Rbsp::default();
		w.bits(4, 0); // sps_video_parameter_set_id
		w.bits(3, 0); // sps_max_sub_layers_minus1
		w.flag(true); // sps_temporal_id_nesting_flag
		self.profile_tier_level(&mut w);
		w.ue(0); // sps_seq_parameter_set_id
		w.ue(1); // chroma_format_idc: 4:2:0
		w.ue(self.coded.0);
		w.ue(self.coded.1);
		// The conformance window crops the coded size back to the frame, in
		// chroma samples, so in units of two luma samples each way.
		let crop = (self.coded.0 - config.width, self.coded.1 - config.height);
		w.flag(crop != (0, 0));
		if crop != (0, 0) {
			w.ue(0);
			w.ue(crop.0 / 2);
			w.ue(0);
			w.ue(crop.1 / 2);
		}
		w.ue(0); // bit_depth_luma_minus8
		w.ue(0); // bit_depth_chroma_minus8
		w.ue(LOG2_MAX_POC_LSB_MINUS4);
		w.flag(false); // sps_sub_layer_ordering_info_present_flag
		ordering(&mut w);
		w.ue(tools.log2_min_cb - 3);
		w.ue(tools.log2_ctb - tools.log2_min_cb);
		w.ue(tools.log2_min_tb - 2);
		w.ue(tools.log2_max_tb - tools.log2_min_tb);
		w.ue(tools.depth_inter);
		w.ue(tools.depth_intra);
		w.flag(false); // scaling_list_enabled_flag
		w.flag(tools.amp);
		w.flag(tools.sao);
		w.flag(false); // pcm_enabled_flag
		w.ue(0); // num_short_term_ref_pic_sets: each slice carries its own
		w.flag(false); // long_term_ref_pics_present_flag
		w.flag(tools.temporal_mvp);
		w.flag(false); // strong_intra_smoothing_enabled_flag
		w.flag(true); // vui_parameters_present_flag
		self.vui(&mut w, config);
		w.flag(false); // sps_extension_present_flag
		w.trailing();
		nal(SPS, &w.finish())
	}

	/// Writes the VUI, H.265 E.2.1: the color space, the frame rate, and the restrictions IPPP coding keeps.
	fn vui(&self, w: &mut Rbsp, config: &Config) {
		let (primaries, transfer, matrix) = config.color.vui();
		w.flag(false); // aspect_ratio_info_present_flag
		w.flag(false); // overscan_info_present_flag
		w.flag(true); // video_signal_type_present_flag
		w.bits(3, 5); // video_format: unspecified
		w.flag(config.color.full_range);
		w.flag(true); // colour_description_present_flag
		w.bits(8, primaries.into());
		w.bits(8, transfer.into());
		w.bits(8, matrix.into());
		w.flag(false); // chroma_loc_info_present_flag
		w.flag(false); // neutral_chroma_indication_flag
		w.flag(false); // field_seq_flag
		w.flag(false); // frame_field_info_present_flag
		w.flag(false); // default_display_window_flag
		w.flag(true); // vui_timing_info_present_flag
		timing(w, config.framerate);
		w.flag(false); // vui_hrd_parameters_present_flag
		w.flag(true); // bitstream_restriction_flag
		w.flag(false); // tiles_fixed_structure_flag
		w.flag(true); // motion_vectors_over_pic_boundaries_flag
		w.flag(true); // restricted_ref_pic_lists_flag
		w.ue(0); // min_spatial_segmentation_idc
		w.ue(0); // max_bytes_per_pic_denom
		w.ue(0); // max_bits_per_min_cu_denom
		w.ue(15); // log2_max_mv_length_horizontal
		w.ue(15); // log2_max_mv_length_vertical
	}

	/// Writes the picture parameter set, H.265 7.3.2.3.
	fn pps(&self) -> Vec<u8> {
		let tools = &self.tools;
		let mut w = Rbsp::default();
		w.ue(0); // pps_pic_parameter_set_id
		w.ue(0); // pps_seq_parameter_set_id
		w.flag(false); // dependent_slice_segments_enabled_flag
		w.flag(false); // output_flag_present_flag
		w.bits(3, 0); // num_extra_slice_header_bits
		w.flag(false); // sign_data_hiding_enabled_flag
		w.flag(false); // cabac_init_present_flag
		w.ue(0); // num_ref_idx_l0_default_active_minus1
		w.ue(0); // num_ref_idx_l1_default_active_minus1
		w.se(INIT_QP - 26);
		w.flag(false); // constrained_intra_pred_flag
		w.flag(tools.transform_skip);
		w.flag(tools.cu_qp_delta);
		if tools.cu_qp_delta {
			w.ue(self.diff_cu_qp_delta_depth());
		}
		w.se(0); // pps_cb_qp_offset
		w.se(0); // pps_cr_qp_offset
		w.flag(false); // pps_slice_chroma_qp_offsets_present_flag
		w.flag(false); // weighted_pred_flag
		w.flag(false); // weighted_bipred_flag
		w.flag(false); // transquant_bypass_enabled_flag
		w.flag(false); // tiles_enabled_flag
		w.flag(false); // entropy_coding_sync_enabled_flag
		w.flag(true); // pps_loop_filter_across_slices_enabled_flag
		w.flag(false); // deblocking_filter_control_present_flag
		w.flag(false); // pps_scaling_list_data_present_flag
		w.flag(false); // lists_modification_present_flag
		w.ue(0); // log2_parallel_merge_level_minus2
		w.flag(false); // slice_segment_header_extension_present_flag
		w.flag(false); // pps_extension_present_flag
		w.trailing();
		nal(PPS, &w.finish())
	}

	/// Writes the header of a picture's one slice segment, H.265 7.3.6.1.
	///
	/// A P or GPB picture refers to the one before it, so its short-term
	/// reference set is that single picture, one order count back.
	fn slice_header(&self, nal_type: u8, slice_type: u8, count: u32) -> Vec<u8> {
		let tools = &self.tools;
		let mut w = Rbsp::default();
		w.flag(true); // first_slice_segment_in_pic_flag
		if nal_type == IDR_W_RADL {
			w.flag(false); // no_output_of_prior_pics_flag
		}
		w.ue(0); // slice_pic_parameter_set_id
		w.ue(slice_type.into());
		if nal_type != IDR_W_RADL {
			let lsb_bits = LOG2_MAX_POC_LSB_MINUS4 + 4;
			w.bits(lsb_bits, count & ((1 << lsb_bits) - 1));
			w.flag(false); // short_term_ref_pic_set_sps_flag
				  // st_ref_pic_set(0): no inter prediction of the set at index 0.
			w.ue(1); // num_negative_pics
			w.ue(0); // num_positive_pics
			w.ue(0); // delta_poc_s0_minus1: the previous picture
			w.flag(true); // used_by_curr_pic_s0_flag
			if tools.temporal_mvp {
				w.flag(true); // slice_temporal_mvp_enabled_flag
			}
		}
		if tools.sao {
			w.flag(true); // slice_sao_luma_flag
			w.flag(true); // slice_sao_chroma_flag
		}
		if slice_type != SLICE_I {
			w.flag(false); // num_ref_idx_active_override_flag
			if slice_type == SLICE_B {
				w.flag(false); // mvd_l1_zero_flag
			}
			if tools.temporal_mvp && slice_type == SLICE_B {
				w.flag(true); // collocated_from_l0_flag
			}
			w.ue(0); // five_minus_max_num_merge_cand
		}
		w.se(0); // slice_qp_delta
		   // Loop filtering is on, so this is present; with one slice it filters nothing.
		w.flag(false); // slice_loop_filter_across_slices_enabled_flag
		w.byte_alignment();
		nal(nal_type, &w.finish())
	}
}

/// Returns an empty reference slot.
fn invalid() -> PictureHEVC {
	PictureHEVC::new(VA_INVALID_ID, 0, VA_PICTURE_HEVC_INVALID)
}

/// Writes the one set of sub-layer ordering limits, for IPPP coding: one reference and the current picture, nothing reordered.
fn ordering(w: &mut Rbsp) {
	w.ue(1); // max_dec_pic_buffering_minus1
	w.ue(0); // max_num_reorder_pics
	w.ue(0); // max_latency_increase_plus1
}

/// Returns the surface alignment the driver encodes at, in samples across and down: 1 where it names none.
///
/// radeonsi encodes a picture rounded up to it (64 by 16) and rewrites the SPS
/// at that size, keeping our conformance window, so a coded size short of it
/// would decode with the wrong crop.
fn alignment(va_config: &mut VaConfig) -> (u32, u32) {
	let named = va_config
		.query_surface_attributes_by_type(VASurfaceAttribType::VASurfaceAttribAlignmentSize)
		.ok()
		.and_then(|values| values.into_iter().next());
	match named {
		// VASurfaceAttribAlignmentStruct: log2 width in bits 0-3, log2 height in bits 4-7.
		Some(GenericValue::Integer(value)) => (1 << (value & 0xf), 1 << ((value >> 4) & 0xf)),
		_ => (1, 1),
	}
}

/// Writes the timing a VPS and a VUI share: one tick per frame, and an order count step per picture.
fn timing(w: &mut Rbsp, framerate: u32) {
	w.bits(32, 1); // num_units_in_tick
	w.bits(32, framerate.max(1)); // time_scale
	w.flag(true); // poc_proportional_to_timing_flag
	w.ue(0); // num_ticks_poc_diff_one_minus1
}

/// Returns `general_level_idc` for the lowest Main tier level whose picture size and sample rate cover a `coded` picture at `framerate`.
///
/// Only those limits, as for H.264: the level's maximum bitrate is not
/// checked, since the bitrate can change while the SPS cannot.
fn level((width, height): (u32, u32), framerate: u32) -> Option<u8> {
	// H.265 Table A.8: (level_idc, MaxLumaPs, MaxLumaSr).
	const LEVELS: [(u8, u64, u64); 13] = [
		(30, 36_864, 552_960),
		(60, 122_880, 3_686_400),
		(63, 245_760, 7_372_800),
		(90, 552_960, 16_588_800),
		(93, 983_040, 33_177_600),
		(120, 2_228_224, 66_846_720),
		(123, 2_228_224, 133_693_440),
		(150, 8_912_896, 267_386_880),
		(153, 8_912_896, 534_773_760),
		(156, 8_912_896, 1_069_547_520),
		(180, 35_651_584, 1_069_547_520),
		(183, 35_651_584, 2_139_095_040),
		(186, 35_651_584, 4_278_190_080),
	];
	let size = u64::from(width) * u64::from(height);
	let rate = size * u64::from(framerate.max(1));
	// A.4.1: neither side may exceed sqrt(8 * MaxLumaPs).
	let side = u64::from(width.max(height)).pow(2);
	LEVELS
		.into_iter()
		.find(|&(_, max_size, max_rate)| size <= max_size && side <= 8 * max_size && rate <= max_rate)
		.map(|(level, _, _)| level)
}

/// Writes NAL unit type `kind` behind a four-byte start code, with emulation prevention.
fn nal(kind: u8, rbsp: &[u8]) -> Vec<u8> {
	// nal_unit_header: forbidden_zero_bit, nal_unit_type, nuh_layer_id 0, nuh_temporal_id_plus1 1.
	let mut out = vec![0, 0, 0, 1, kind << 1, 1];
	let mut zeros = 0;
	for &byte in rbsp {
		if zeros == 2 && byte <= 3 {
			out.push(3);
			zeros = 0;
		}
		out.push(byte);
		zeros = if byte == 0 { zeros + 1 } else { 0 };
	}
	out
}

/// A raw byte sequence payload, written a field at a time.
#[derive(Default)]
struct Rbsp {
	bytes: Vec<u8>,
	/// The partial byte, its bits filled from the top.
	current: u8,
	/// How many bits of `current` are filled.
	filled: u32,
}

impl Rbsp {
	/// Writes the low `count` bits of `value`, most significant first.
	fn bits(&mut self, count: u32, value: u32) {
		debug_assert!(
			count <= 32 && (count == 32 || value >> count == 0),
			"{value} in {count} bits"
		);
		for bit in (0..count).rev() {
			self.flag(value >> bit & 1 == 1);
		}
	}

	fn flag(&mut self, bit: bool) {
		self.current |= (bit as u8) << (7 - self.filled);
		self.filled += 1;
		if self.filled == 8 {
			self.bytes.push(self.current);
			self.current = 0;
			self.filled = 0;
		}
	}

	/// Writes `value` as an unsigned Exp-Golomb code, H.265 9.2.
	fn ue(&mut self, value: u32) {
		let coded = u64::from(value) + 1;
		let length = 64 - coded.leading_zeros();
		self.bits(length - 1, 0);
		for bit in (0..length).rev() {
			self.flag(coded >> bit & 1 == 1);
		}
	}

	/// Writes `value` as a signed Exp-Golomb code, H.265 9.2.2.
	fn se(&mut self, value: i32) {
		let mapped = match value > 0 {
			true => 2 * value.unsigned_abs() - 1,
			false => 2 * value.unsigned_abs(),
		};
		self.ue(mapped);
	}

	/// Writes `rbsp_trailing_bits`: a one, then zeros to the byte boundary.
	fn trailing(&mut self) {
		self.flag(true);
		while self.filled != 0 {
			self.flag(false);
		}
	}

	/// Writes `byte_alignment` at the end of a slice header, which is the same bits.
	fn byte_alignment(&mut self) {
		self.trailing();
	}

	fn finish(self) -> Vec<u8> {
		debug_assert_eq!(self.filled, 0, "an RBSP ends on a byte boundary");
		self.bytes
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn written(write: impl FnOnce(&mut Rbsp)) -> Vec<u8> {
		let mut w = Rbsp::default();
		write(&mut w);
		w.trailing();
		w.finish()
	}

	#[test]
	fn exp_golomb_codes_match_the_spec_table() {
		// 9.2: 0 -> 1, 1 -> 010, 2 -> 011, 3 -> 00100, then the stop bit.
		assert_eq!(written(|w| w.ue(0)), [0b1100_0000]);
		assert_eq!(written(|w| w.ue(1)), [0b0101_0000]);
		assert_eq!(written(|w| w.ue(2)), [0b0111_0000]);
		assert_eq!(written(|w| w.ue(3)), [0b0010_0100]);
		// 9.2.2: k > 0 maps to 2k - 1, k <= 0 to -2k.
		assert_eq!(written(|w| w.se(1)), written(|w| w.ue(1)));
		assert_eq!(written(|w| w.se(-1)), written(|w| w.ue(2)));
		assert_eq!(written(|w| w.se(-26)), written(|w| w.ue(52)));
		assert_eq!(written(|w| w.ue(u32::MAX - 1)).len(), 8, "63 bits and the stop bit");
		assert_eq!(written(|w| w.ue(u32::MAX)).len(), 9, "65 bits and the stop bit");
	}

	/// 7.4.2: two zeros then a byte below 4 take an emulation prevention byte,
	/// and the zeros count again from the byte after it.
	#[test]
	fn start_code_patterns_are_escaped() {
		assert_eq!(
			nal(PPS, &[0, 0, 1, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0]),
			[0, 0, 0, 1, 0x44, 1, 0, 0, 3, 1, 0, 0, 3, 0, 0, 4, 0, 0, 3, 0, 0, 3, 0]
		);
	}

	#[test]
	fn the_level_covers_the_picture_size_and_sample_rate() {
		assert_eq!(level((640, 360), 30), Some(63));
		assert_eq!(level((1280, 720), 30), Some(93));
		assert_eq!(level((1920, 1080), 30), Some(120));
		assert_eq!(level((1920, 1080), 60), Some(123));
		assert_eq!(level((3840, 2160), 30), Some(150));
		assert_eq!(level((3840, 2160), 60), Some(153));
		// A sliver within level 3's area but longer than its widest side, and level 3.1's.
		assert_eq!(level((4096, 64), 30), Some(120));
		assert_eq!(level((16384, 16384), 30), None);
	}

	/// The coded size rounds up to whole minimum coding blocks, and the SPS crops it back.
	#[test]
	fn the_coded_size_is_whole_coding_blocks() {
		let config = Config::new(852, 478, 30, 1_000_000, 30);
		let stream = Stream::with_tools(Tools::FALLBACK, (1, 1), &config).unwrap();
		assert_eq!(stream.coded(), (864, 480));
		let tools = Tools {
			log2_min_cb: 3,
			..Tools::FALLBACK
		};
		assert_eq!(Stream::with_tools(tools, (1, 1), &config).unwrap().coded(), (856, 480));
		// radeonsi's 64 by 16 surface alignment.
		assert_eq!(
			Stream::with_tools(tools, (64, 16), &config).unwrap().coded(),
			(896, 480)
		);
	}
}
