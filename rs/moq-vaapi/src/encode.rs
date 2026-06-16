//! Thin VA-API H.264 encoder built on the vendored libva binding.
//!
//! Reuses the backend-agnostic bitstream layer ([`crate::codec::h264`]) from
//! discord/cros-codecs (BSD-3-Clause) for SPS/PPS/slice synthesis, and drives
//! libva directly for surface upload + slice submission, rather than vendoring
//! cros-codecs's generic multi-backend encoder framework. The per-frame VA
//! buffer population is ported from cros-codecs's `encoder/stateless/h264/vaapi.rs`.
//!
//! Low-latency only: IPPP (no B-frames), one reference frame, matching the
//! VideoToolbox / Media Foundation / NVENC backends in moq-video.
//!
//! NOTE: compile-verified against libva 1.23, but the emitted bitstream has not
//! been validated at playback. The per-frame param population, the reconstructed
//! surface / reference rotation, and NV12 stride handling are the spots most
//! likely to need hardware tuning.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::codec::h264::parser::SpsBuilder;
use crate::codec::h264::parser::{Level, Pps, PpsBuilder, Profile, SliceHeader, SliceHeaderBuilder, SliceType, Sps};
use crate::codec::h264::synthesizer::Synthesizer;
use crate::{
	BufferType, Config as VaConfig, Context, Display, EncCodedBuffer, EncMiscParameter, EncMiscParameterFrameRate,
	EncMiscParameterRateControl, EncPackedHeaderParameter, EncPackedHeaderType, EncPictureParameter,
	EncPictureParameterBufferH264, EncSequenceParameter, EncSequenceParameterBufferH264, EncSliceParameter,
	EncSliceParameterBufferH264, H264EncFrameCropOffsets, H264EncPicFields, H264EncSeqFields, H264VuiFields, Image,
	MappedCodedBuffer, Picture, PictureH264, RcFlags, Surface, UsageHint, VAConfigAttrib, VAConfigAttribType,
	VAEntrypoint, VAProfile, VA_FOURCC_NV12, VA_INVALID_ID, VA_PICTURE_H264_INVALID,
	VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RC_CBR, VA_RT_FORMAT_YUV420,
};

/// Whether a frame is used as a reference, and for how long.
/// (Vendored from discord/cros-codecs, BSD-3-Clause; used by the slice synthesizer.)
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum IsReference {
	No,
	ShortTerm,
	LongTerm,
}

const MIN_QP: u8 = 1;
const MAX_QP: u8 = 51;
/// `max_frame_num` upper bound for the low-delay GOP; matches cros-codecs.
const LIMIT: u32 = 2048;

/// H.264 encoder configuration. Dimensions must be even (4:2:0 chroma).
#[derive(Clone, Debug)]
pub struct Config {
	pub width: u32,
	pub height: u32,
	pub framerate: u32,
	/// Target bitrate in bits per second (CBR).
	pub bitrate: u32,
	/// Keyframe interval in frames.
	pub gop: u32,
	/// DRM render node to open (e.g. `/dev/dri/renderD128`).
	pub device: PathBuf,
}

impl Config {
	pub fn new(width: u32, height: u32, framerate: u32, bitrate: u32, gop: u32) -> Self {
		Self {
			width,
			height,
			framerate,
			bitrate,
			gop,
			device: PathBuf::from("/dev/dri/renderD128"),
		}
	}
}

/// Per-frame reference metadata. (Mirrors cros-codecs `DpbEntryMeta`.)
#[derive(Copy, Clone)]
struct FrameMeta {
	poc: u16,
	frame_num: u32,
}

/// A VA-API H.264 encoder. Built once, fed NV12 frames, emits Annex-B H.264.
pub struct Encoder {
	config: Config,
	sps: Rc<Sps>,
	pps: Rc<Pps>,
	width_mbs: u16,
	height_mbs: u16,

	// Source surface (reused; NV12 re-uploaded per frame) and a 2-deep
	// reconstructed surface ring (current + previous reference for IPPP).
	input: Option<Surface<()>>,
	recon: Vec<Surface<()>>,
	/// Reference from the previous encoded frame: (recon index, metadata).
	reference: Option<(usize, FrameMeta)>,

	counter: u32,

	// Drop order: encode resources before the display they belong to.
	coded: EncCodedBuffer,
	context: Rc<Context>,
	_va_config: VaConfig,
	display: Arc<Display>,
}

impl Encoder {
	pub fn new(config: Config) -> anyhow::Result<Self> {
		let display = Display::open_drm_display(&config.device)
			.map_err(|e| anyhow::anyhow!("open DRM display {:?}: {e:?}", config.device))?;

		let attrs = vec![
			VAConfigAttrib {
				type_: VAConfigAttribType::VAConfigAttribRTFormat,
				value: VA_RT_FORMAT_YUV420,
			},
			VAConfigAttrib {
				type_: VAConfigAttribType::VAConfigAttribRateControl,
				value: VA_RC_CBR,
			},
		];
		let va_config = display
			.create_config(attrs, VAProfile::VAProfileH264Main, VAEntrypoint::VAEntrypointEncSlice)
			.map_err(|e| anyhow::anyhow!("create VA config: {e:?}"))?;

		let context = display
			.create_context::<()>(&va_config, config.width, config.height, None, true)
			.map_err(|e| anyhow::anyhow!("create VA context: {e:?}"))?;

		let coded_size = (config.bitrate as usize / 4).max(1_500_000);
		let coded = context
			.create_enc_coded(coded_size)
			.map_err(|e| anyhow::anyhow!("create coded buffer: {e:?}"))?;

		let input = make_surface(&display, &config)?;
		let recon = vec![make_surface(&display, &config)?, make_surface(&display, &config)?];

		let (sps, pps) = build_sps_pps(&config);
		let width_mbs = sps.pic_width_in_mbs_minus1 + 1;
		let height_mbs = sps.pic_height_in_map_units_minus1 + 1;

		log::info!(
			"opened VA-API H.264 encoder: {}x{} @ {}fps, {} bps",
			config.width,
			config.height,
			config.framerate,
			config.bitrate
		);
		Ok(Self {
			config,
			sps,
			pps,
			width_mbs,
			height_mbs,
			input: Some(input),
			recon,
			reference: None,
			counter: 0,
			coded,
			context,
			_va_config: va_config,
			display,
		})
	}

	/// Encode one tightly-packed NV12 frame, returning Annex-B H.264 (with inline
	/// SPS/PPS on each IDR). `keyframe` forces an IDR.
	pub fn encode_nv12(&mut self, nv12: &[u8], keyframe: bool) -> anyhow::Result<Vec<u8>> {
		let is_idr = keyframe || self.counter % self.config.gop == 0;
		if is_idr {
			// IDR resets the H.264 frame numbering and the reference list.
			self.counter = 0;
			self.reference = None;
		}

		let input = self.input.take().expect("input surface present");
		upload_nv12(&self.display, &input, self.config.width, self.config.height, nv12)?;

		let meta = FrameMeta {
			poc: ((self.counter * 2) & 0xffff) as u16,
			frame_num: self.counter,
		};
		let recon_idx = (self.counter as usize) % self.recon.len();

		let slice_type = if is_idr { SliceType::I } else { SliceType::P };
		let header = SliceHeaderBuilder::new(&self.pps)
			.slice_type(slice_type)
			.first_mb_in_slice(0)
			.frame_num(meta.frame_num as u16)
			.pic_order_cnt_lsb(meta.poc)
			.build();

		let num_macroblocks = self.width_mbs as u32 * self.height_mbs as u32;
		let bits_per_second = self.config.bitrate;

		// Reference (previous reconstructed frame) for P slices: surface id + meta.
		let reference = match (is_idr, self.reference) {
			(false, Some((ref_idx, ref_meta))) => Some((self.recon[ref_idx].id(), ref_meta)),
			_ => None,
		};

		let seq_param = build_enc_seq_param(&self.sps, bits_per_second, LIMIT, 0);
		let pic_param = build_enc_pic_param(
			&self.pps,
			&self.coded,
			self.recon[recon_idx].id(),
			meta,
			is_idr,
			reference,
		);
		let slice_param = build_enc_slice_param(&self.pps, &header, reference, num_macroblocks);

		let mut picture = Picture::new(meta.frame_num as u64, Rc::clone(&self.context), input);

		// VA-API spec buffer order: sequence, picture, slice, then packed headers,
		// then rate-control misc params.
		picture.add_buffer(self.create(seq_param)?);
		picture.add_buffer(self.create(pic_param)?);
		picture.add_buffer(self.create(slice_param)?);

		if is_idr {
			let (sps_param, sps_data) = packed_header(EncPackedHeaderType::Sequence, &self.packed_sps()?);
			let (pps_param, pps_data) = packed_header(EncPackedHeaderType::Picture, &self.packed_pps()?);
			picture.add_buffer(self.create(sps_param)?);
			picture.add_buffer(self.create(sps_data)?);
			picture.add_buffer(self.create(pps_param)?);
			picture.add_buffer(self.create(pps_data)?);
		}

		let rc = EncMiscParameterRateControl::new(
			bits_per_second,
			100,                                 // target_percentage (CBR)
			self.config.framerate.max(1) * 1000, // window_size (ms)
			26,                                  // initial_qp
			MIN_QP as u32,
			0, // basic_unit_size
			RcFlags::default(),
			0, // icq_quality_factor
			MAX_QP as u32,
			0, // quality_factor
			0, // target_frame_size
		);
		picture.add_buffer(self.create(BufferType::EncMiscParameter(EncMiscParameter::RateControl(rc)))?);

		let framerate = EncMiscParameterFrameRate::new(self.config.framerate, 0);
		picture.add_buffer(self.create(BufferType::EncMiscParameter(EncMiscParameter::FrameRate(framerate)))?);

		let picture = picture.begin().map_err(|e| anyhow::anyhow!("picture begin: {e:?}"))?;
		let picture = picture.render().map_err(|e| anyhow::anyhow!("picture render: {e:?}"))?;
		let picture = picture.end().map_err(|e| anyhow::anyhow!("picture end: {e:?}"))?;
		// Sync (PictureEnd -> PictureSync) so the surface is reclaimable and the
		// coded buffer is ready to read.
		let picture = picture
			.sync()
			.map_err(|(e, _)| anyhow::anyhow!("picture sync: {e:?}"))?;

		// Reclaim the source surface for the next frame.
		self.input = Some(
			picture
				.take_surface()
				.map_err(|_| anyhow::anyhow!("reclaim input surface (still referenced)"))?,
		);

		// The reconstructed surface syncs implicitly; read the coded bitstream.
		let bitstream = self.read_coded()?;

		self.reference = Some((recon_idx, meta));
		self.counter += 1;
		Ok(bitstream)
	}

	fn create(&self, buffer: BufferType) -> anyhow::Result<crate::Buffer> {
		self.context
			.create_buffer(buffer)
			.map_err(|e| anyhow::anyhow!("create VA buffer: {e:?}"))
	}

	fn packed_sps(&self) -> anyhow::Result<Vec<u8>> {
		let mut buf = Vec::new();
		Synthesizer::<'_, Sps, _>::synthesize(3, &self.sps, &mut buf, true)
			.map_err(|e| anyhow::anyhow!("synthesize SPS: {e:?}"))?;
		Ok(buf)
	}

	fn packed_pps(&self) -> anyhow::Result<Vec<u8>> {
		let mut buf = Vec::new();
		Synthesizer::<'_, Pps, _>::synthesize(3, &self.pps, &mut buf, true)
			.map_err(|e| anyhow::anyhow!("synthesize PPS: {e:?}"))?;
		Ok(buf)
	}

	fn read_coded(&self) -> anyhow::Result<Vec<u8>> {
		let mapped = MappedCodedBuffer::new(&self.coded).map_err(|e| anyhow::anyhow!("map coded buffer: {e:?}"))?;
		let mut out = Vec::new();
		for segment in mapped.iter() {
			out.extend_from_slice(segment.buf);
		}
		Ok(out)
	}
}

/// Allocate one NV12 encode surface.
fn make_surface(display: &Arc<Display>, config: &Config) -> anyhow::Result<Surface<()>> {
	let mut surfaces = display
		.create_surfaces::<()>(
			VA_RT_FORMAT_YUV420,
			Some(VA_FOURCC_NV12),
			config.width,
			config.height,
			Some(UsageHint::USAGE_HINT_ENCODER),
			vec![()],
		)
		.map_err(|e| anyhow::anyhow!("create surface: {e:?}"))?;
	surfaces.pop().ok_or_else(|| anyhow::anyhow!("no surface created"))
}

/// Upload tightly-packed NV12 into a surface. Ported from cros-codecs
/// `upload_nv12_img` (honors the image's per-plane offsets + pitches).
fn upload_nv12(
	display: &Arc<Display>,
	surface: &Surface<()>,
	width: u32,
	height: u32,
	data: &[u8],
) -> anyhow::Result<()> {
	let formats = display
		.query_image_formats()
		.map_err(|e| anyhow::anyhow!("query image formats: {e:?}"))?;
	let format = formats
		.into_iter()
		.find(|f| f.fourcc == VA_FOURCC_NV12)
		.ok_or_else(|| anyhow::anyhow!("driver has no NV12 image format"))?;

	let mut image = Image::create_from(surface, format, surface.size(), surface.size())
		.map_err(|e| anyhow::anyhow!("create image: {e:?}"))?;
	let va_image = *image.image();
	let dest: &mut [u8] = image.as_mut();
	let (w, h) = (width as usize, height as usize);

	// Luma plane.
	let mut src = data;
	let mut dst = &mut dest[va_image.offsets[0] as usize..];
	for _ in 0..h {
		dst[..w].copy_from_slice(&src[..w]);
		dst = &mut dst[va_image.pitches[0] as usize..];
		src = &src[w..];
	}
	// Interleaved chroma plane (h/2 rows of w bytes).
	let mut src = &data[w * h..];
	let mut dst = &mut dest[va_image.offsets[1] as usize..];
	for _ in 0..h / 2 {
		dst[..w].copy_from_slice(&src[..w]);
		dst = &mut dst[va_image.pitches[1] as usize..];
		src = &src[w..];
	}

	drop(image);
	surface.sync().map_err(|e| anyhow::anyhow!("surface sync: {e:?}"))?;
	Ok(())
}

/// Build the SPS/PPS for a low-latency IPPP stream. Ported from
/// discord/cros-codecs `LowDelayH264Delegate::new_sequence` (BSD-3-Clause).
fn build_sps_pps(config: &Config) -> (Rc<Sps>, Rc<Pps>) {
	let level = Level::L4;
	let sps = SpsBuilder::new()
		.seq_parameter_set_id(0)
		.profile_idc(Profile::Main)
		.chroma_format_idc(1)
		.level_idc(level)
		.max_frame_num(LIMIT)
		.pic_order_cnt_type(0)
		.max_pic_order_cnt_lsb(LIMIT * 2)
		.max_num_ref_frames(1)
		.frame_mbs_only_flag(true)
		.direct_8x8_inference_flag(level >= Level::L3)
		.resolution(config.width, config.height)
		.bit_depth_luma(8)
		.bit_depth_chroma(8)
		.aspect_ratio(1, 1)
		.timing_info(1, config.framerate * 2, false)
		.max_num_reorder_frames(0)
		.max_dec_frame_buffering(1)
		.build();

	let pps = PpsBuilder::new(Rc::clone(&sps))
		.pic_parameter_set_id(0)
		.pic_init_qp(26)
		.entropy_coding_mode_flag(true)
		.transform_8x8_mode_flag(false)
		.deblocking_filter_control_present_flag(true)
		.num_ref_idx_l0_default_active(1)
		.num_ref_idx_l1_default_active_minus1(0)
		.build();

	(sps, pps)
}

fn build_invalid_pic() -> PictureH264 {
	PictureH264::new(VA_INVALID_ID, 0, VA_PICTURE_H264_INVALID, 0, 0)
}

fn build_h264_pic(surface_id: u32, meta: FrameMeta) -> PictureH264 {
	PictureH264::new(
		surface_id,
		meta.frame_num,
		VA_PICTURE_H264_SHORT_TERM_REFERENCE,
		meta.poc as i32,
		meta.poc as i32,
	)
}

/// Ported from cros-codecs `build_enc_seq_param` (BSD-3-Clause).
fn build_enc_seq_param(sps: &Sps, bits_per_second: u32, intra_period: u32, ip_period: u32) -> BufferType {
	let seq_fields = H264EncSeqFields::new(
		sps.chroma_format_idc as u32,
		sps.frame_mbs_only_flag as u32,
		sps.mb_adaptive_frame_field_flag as u32,
		sps.seq_scaling_matrix_present_flag as u32,
		sps.direct_8x8_inference_flag as u32,
		sps.log2_max_frame_num_minus4 as u32,
		sps.pic_order_cnt_type as u32,
		sps.log2_max_pic_order_cnt_lsb_minus4 as u32,
		sps.delta_pic_order_always_zero_flag as u32,
	);

	let frame_crop = if sps.frame_cropping_flag {
		Some(H264EncFrameCropOffsets::new(
			sps.frame_crop_left_offset,
			sps.frame_crop_right_offset,
			sps.frame_crop_top_offset,
			sps.frame_crop_bottom_offset,
		))
	} else {
		None
	};

	let vui_fields = if sps.vui_parameters_present_flag {
		Some(H264VuiFields::new(
			sps.vui_parameters.aspect_ratio_idc as u32,
			sps.vui_parameters.timing_info_present_flag as u32,
			sps.vui_parameters.bitstream_restriction_flag as u32,
			sps.vui_parameters.log2_max_mv_length_horizontal,
			sps.vui_parameters.log2_max_mv_length_vertical,
			sps.vui_parameters.fixed_frame_rate_flag as u32,
			sps.vui_parameters.low_delay_hrd_flag as u32,
			sps.vui_parameters.motion_vectors_over_pic_boundaries_flag as u32,
		))
	} else {
		None
	};

	let mut offset_for_ref_frame = [0i32; 256];
	offset_for_ref_frame[..255].copy_from_slice(&sps.offset_for_ref_frame[..]);

	BufferType::EncSequenceParameter(EncSequenceParameter::H264(EncSequenceParameterBufferH264::new(
		sps.seq_parameter_set_id,
		sps.level_idc as u8,
		intra_period,
		intra_period,
		ip_period,
		bits_per_second,
		sps.max_num_ref_frames as u32,
		sps.pic_width_in_mbs_minus1 + 1,
		sps.pic_height_in_map_units_minus1 + 1,
		&seq_fields,
		sps.bit_depth_luma_minus8,
		sps.bit_depth_chroma_minus8,
		sps.num_ref_frames_in_pic_order_cnt_cycle,
		sps.offset_for_non_ref_pic,
		sps.offset_for_top_to_bottom_field,
		offset_for_ref_frame,
		frame_crop,
		vui_fields,
		sps.vui_parameters.aspect_ratio_idc,
		sps.vui_parameters.sar_width as u32,
		sps.vui_parameters.sar_height as u32,
		sps.vui_parameters.num_units_in_tick,
		sps.vui_parameters.time_scale,
	)))
}

/// Ported from cros-codecs `build_enc_pic_param` (BSD-3-Clause).
fn build_enc_pic_param(
	pps: &Pps,
	coded: &EncCodedBuffer,
	recon_id: u32,
	meta: FrameMeta,
	is_idr: bool,
	reference: Option<(u32, FrameMeta)>,
) -> BufferType {
	let pic_fields = H264EncPicFields::new(
		is_idr as u32,
		1, // reference_pic_flag: this frame is a short-term reference
		pps.entropy_coding_mode_flag as u32,
		pps.weighted_pred_flag as u32,
		pps.weighted_bipred_idc as u32,
		pps.constrained_intra_pred_flag as u32,
		pps.transform_8x8_mode_flag as u32,
		pps.deblocking_filter_control_present_flag as u32,
		pps.redundant_pic_cnt_present_flag as u32,
		0,
		pps.pic_scaling_matrix_present_flag as u32,
	);

	let curr_pic = build_h264_pic(recon_id, meta);
	let mut reference_frames: [PictureH264; 16] = std::array::from_fn(|_| build_invalid_pic());
	if let Some((id, m)) = reference {
		reference_frames[0] = build_h264_pic(id, m);
	}

	BufferType::EncPictureParameter(EncPictureParameter::H264(EncPictureParameterBufferH264::new(
		curr_pic,
		reference_frames,
		coded.id(),
		pps.pic_parameter_set_id,
		pps.seq_parameter_set_id,
		0,
		meta.frame_num as u16,
		(pps.pic_init_qp_minus26 + 26) as u8,
		pps.num_ref_idx_l0_default_active_minus1,
		pps.num_ref_idx_l1_default_active_minus1,
		pps.chroma_qp_index_offset,
		pps.second_chroma_qp_index_offset,
		&pic_fields,
	)))
}

/// Ported from cros-codecs `build_enc_slice_param` (BSD-3-Clause), simplified for
/// IPPP (no weighted prediction, single reference in list 0).
fn build_enc_slice_param(
	pps: &Pps,
	header: &SliceHeader,
	reference: Option<(u32, FrameMeta)>,
	num_macroblocks: u32,
) -> BufferType {
	let mut ref_pic_list_0: [PictureH264; 32] = std::array::from_fn(|_| build_invalid_pic());
	if let Some((id, m)) = reference {
		ref_pic_list_0[0] = build_h264_pic(id, m);
	}
	let ref_pic_list_1: [PictureH264; 32] = std::array::from_fn(|_| build_invalid_pic());

	let (num_ref_idx_l0_active_minus1, num_ref_idx_l1_active_minus1) = if header.num_ref_idx_active_override_flag {
		(header.num_ref_idx_l0_active_minus1, header.num_ref_idx_l1_active_minus1)
	} else {
		(
			pps.num_ref_idx_l0_default_active_minus1,
			pps.num_ref_idx_l1_default_active_minus1,
		)
	};

	BufferType::EncSliceParameter(EncSliceParameter::H264(EncSliceParameterBufferH264::new(
		header.first_mb_in_slice,
		num_macroblocks,
		VA_INVALID_ID,
		header.slice_type as u8,
		pps.pic_parameter_set_id,
		header.idr_pic_id,
		header.pic_order_cnt_lsb,
		header.delta_pic_order_cnt_bottom,
		header.delta_pic_order_cnt,
		header.direct_spatial_mv_pred_flag as u8,
		header.num_ref_idx_active_override_flag as u8,
		num_ref_idx_l0_active_minus1,
		num_ref_idx_l1_active_minus1,
		ref_pic_list_0,
		ref_pic_list_1,
		header.pred_weight_table.luma_log2_weight_denom,
		header.pred_weight_table.chroma_log2_weight_denom,
		0,
		header.pred_weight_table.luma_weight_l0,
		[0i16; 32],
		0,
		header.pred_weight_table.chroma_weight_l0,
		[[0i16; 2]; 32],
		0,
		header.pred_weight_table.luma_weight_l1,
		[0i16; 32],
		0,
		header.pred_weight_table.chroma_weight_l1,
		[[0i16; 2]; 32],
		header.cabac_init_idc,
		header.slice_qp_delta,
		header.disable_deblocking_filter_idc,
		header.slice_alpha_c0_offset_div2,
		header.slice_beta_offset_div2,
	)))
}

/// A packed-header parameter + data buffer pair for a synthesized NAL.
fn packed_header(kind: EncPackedHeaderType, data: &[u8]) -> (BufferType, BufferType) {
	let param =
		BufferType::EncPackedHeaderParameter(EncPackedHeaderParameter::new(kind, (data.len() * 8) as u32, true));
	let payload = BufferType::EncPackedHeaderData(data.to_vec());
	(param, payload)
}
