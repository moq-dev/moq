//! H.264 / AVC.
//!
//! Parses SPS NAL units and AVCDecoderConfigurationRecord blobs into
//! catalog-ready fields. The [`Avc1`] transmuxer rewrites Annex-B input
//! (inline SPS/PPS) as length-prefixed NALU + out-of-band avcC, which is
//! what every CMAF and MKV consumer expects. [`Export`] subscribes to a
//! catalog-narrowed H.264 rendition and emits an Annex-B elementary
//! stream; [`Split`] does the byte-level framing for the Annex-B (avc3)
//! wire shape and [`Import`] is the pure frame publisher that resolves the
//! catalog. avc1 (length-prefixed NALU) has no stream framing; wrap one
//! access unit with `avc1_frame`.

mod export;
mod import;
mod split;

pub use export::*;
pub use import::*;
pub use split::*;

use bytes::{Buf, BufMut, Bytes, BytesMut};

// H.264 NAL unit types (ISO/IEC 14496-10 §7.4.1).
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

/// Wrap one avc1 (length-prefixed NALU) access unit as a single
/// [`Frame`](crate::container::Frame), with the keyframe flag set when it
/// carries an IDR slice (NAL type 5).
///
/// avc1 is not a stream: each access unit arrives whole with its NALU
/// `length_size` known out-of-band from the avcC (`super::Avcc::parse(avcc).length_size`).
/// The payload is passed through verbatim.
pub(crate) fn avc1_frame(
	data: impl moq_net::IntoBytes,
	length_size: usize,
	pts: moq_net::Timestamp,
) -> crate::Result<crate::container::Frame> {
	let keyframe = avc1_is_keyframe(data.as_ref(), length_size);
	Ok(crate::container::Frame {
		timestamp: pts,
		payload: data.into_bytes(),
		keyframe,
		duration: None,
	})
}

/// Detect whether an avc1-shaped (length-prefixed) buffer contains an IDR slice.
fn avc1_is_keyframe(data: &[u8], length_size: usize) -> bool {
	let Ok(nals) = crate::codec::annexb::length_prefixed_nals(data, length_size) else {
		return false;
	};
	nals.map_while(std::result::Result::ok)
		.any(|nal| nal.first().is_some_and(|header| header & 0x1f == 5))
}

/// H.264 parsing and transform errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("SPS NAL too short")]
	SpsTooShort,

	#[error("failed to parse SPS")]
	SpsParse,

	#[error("AVCDecoderConfigurationRecord too short")]
	AvccTooShort,

	#[error("AVCDecoderConfigurationRecord truncated")]
	AvccTruncated,

	#[error("avc1 description for rendition {name:?} is missing SPS or PPS (sps={sps}, pps={pps})")]
	MissingParamSets { name: String, sps: usize, pps: usize },

	#[error("SPS too large for avcC length field ({0} > {max})", max = u16::MAX)]
	SpsTooLarge(usize),

	#[error("PPS too large for avcC length field ({0} > {max})", max = u16::MAX)]
	PpsTooLarge(usize),

	#[error("avcC requires at least one SPS")]
	MissingSps,

	#[error("too many SPS for avcC ({0} > 31)")]
	TooManySps(usize),

	#[error("too many PPS for avcC ({0} > 255)")]
	TooManyPps(usize),

	#[error("NAL too large for 4-byte length prefix")]
	NalTooLarge,

	#[error("NAL unit is too short")]
	NalTooShort,

	#[error("forbidden zero bit is not zero")]
	ForbiddenZeroBit,

	#[error("not initialized")]
	NotInitialized,

	#[error("avc3 track not created")]
	Avc3TrackNotCreated,

	#[error("missing timestamp")]
	MissingTimestamp,

	#[error("annexb: {0}")]
	Annexb(#[from] crate::codec::annexb::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Parsed H.264 SPS (Sequence Parameter Set) NAL.
///
/// Wraps [`h264_parser::Sps`] with the codec-config fields that the hang
/// catalog records: profile_idc, level_idc, and the packed constraint_set
/// flags. The first byte of `nal` must be the NAL header.
#[derive(Debug, Clone)]
pub struct Sps {
	pub profile: u8,
	pub constraints: u8,
	pub level: u8,
	pub coded_width: u32,
	pub coded_height: u32,
}

impl Sps {
	/// Parse an SPS NAL unit.
	pub fn parse(nal: &[u8]) -> Result<Self> {
		if nal.len() < 4 {
			return Err(Error::SpsTooShort);
		}
		let rbsp = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
		let sps = h264_parser::Sps::parse(&rbsp).map_err(|_| Error::SpsParse)?;
		Ok(Self {
			profile: sps.profile_idc,
			constraints: pack_constraint_flags(&sps),
			level: sps.level_idc,
			coded_width: sps.width,
			coded_height: sps.height,
		})
	}
}

/// The reordering an SPS NAL unit declares in its VUI: `max_num_reorder_frames` from the
/// bitstream restriction, and the frame period when `fixed_frame_rate_flag` is set.
///
/// `None` when the SPS carries no VUI or no bitstream restriction, or fails to parse.
pub(crate) fn sps_reorder(nal: &[u8]) -> Option<crate::codec::video::Reorder> {
	let vui = sps_vui(nal)?;
	Some(crate::codec::video::Reorder {
		depth: vui.depth?,
		period: vui.period,
	})
}

/// The NAL HRD an SPS NAL unit's VUI declares, when it carries one.
pub(crate) fn sps_hrd(nal: &[u8]) -> Option<crate::codec::video::Hrd> {
	sps_vui(nal)?.hrd
}

/// The frame rate an SPS NAL unit fixes with `fixed_frame_rate_flag`.
///
/// Without the flag the VUI tick only bounds the rate from above, so it is left to measurement.
/// A field-capable SPS is left out too: `pic_struct` in the slice header then decides whether a
/// tick counts a field or a frame (ITU-T H.264 E.2.1, Table E-6), and only measurement settles it.
pub(crate) fn sps_framerate(nal: &[u8]) -> Option<f64> {
	let vui = sps_vui(nal)?;
	if !vui.frame_only {
		return None;
	}
	let (units, scale) = vui.period?;
	Some(scale as f64 / units as f64)
}

/// What an SPS VUI declares about picture timing.
struct Vui {
	/// `frame_mbs_only_flag`: every picture is a frame, so two ticks are one frame.
	frame_only: bool,
	/// One frame's duration as `(units, scale)` when `fixed_frame_rate_flag` is set.
	period: Option<(u64, u64)>,
	/// `max_num_reorder_frames`, when the bitstream restriction is present.
	depth: Option<u32>,
	/// The NAL HRD, when present.
	hrd: Option<crate::codec::video::Hrd>,
}

/// `None` when the SPS carries no VUI or fails to parse.
fn sps_vui(nal: &[u8]) -> Option<Vui> {
	if nal.len() < 4 {
		return None;
	}
	let rbsp = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
	let sps = h264_parser::Sps::parse(&rbsp).ok()?;
	if !sps.vui_parameters_present_flag {
		return None;
	}
	vui(&rbsp).ok().flatten()
}

/// Walk an SPS RBSP to its VUI (ITU-T H.264 7.3.2.1.1, E.1.1). `h264_parser` stops at
/// `vui_parameters_present_flag` without exposing its position, so this re-reads the fields
/// before it.
fn vui(rbsp: &[u8]) -> h264_parser::Result<Option<Vui>> {
	use h264_parser::bitreader::BitReader;
	use h264_parser::eg::{read_se, read_ue};

	let mut r = BitReader::new(rbsp);
	let profile_idc = r.read_u8()?;
	r.skip_bits(16)?; // constraint flags, reserved bits, level_idc
	read_ue(&mut r)?; // seq_parameter_set_id
	if matches!(
		profile_idc,
		100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
	) {
		let chroma_format_idc = read_ue(&mut r)?;
		if chroma_format_idc == 3 {
			r.skip_bits(1)?; // separate_colour_plane_flag
		}
		read_ue(&mut r)?; // bit_depth_luma_minus8
		read_ue(&mut r)?; // bit_depth_chroma_minus8
		r.skip_bits(1)?; // qpprime_y_zero_transform_bypass_flag
		if r.read_flag()? {
			let lists = if chroma_format_idc == 3 { 12 } else { 8 };
			for i in 0..lists {
				if r.read_flag()? {
					let size = if i < 6 { 16 } else { 64 };
					let (mut last, mut next) = (8i32, 8i32);
					for _ in 0..size {
						if next != 0 {
							next = (last + read_se(&mut r)?).rem_euclid(256);
						}
						if next != 0 {
							last = next;
						}
					}
				}
			}
		}
	}
	read_ue(&mut r)?; // log2_max_frame_num_minus4
	match read_ue(&mut r)? {
		0 => {
			read_ue(&mut r)?; // log2_max_pic_order_cnt_lsb_minus4
		}
		1 => {
			r.skip_bits(1)?; // delta_pic_order_always_zero_flag
			read_se(&mut r)?; // offset_for_non_ref_pic
			read_se(&mut r)?; // offset_for_top_to_bottom_field
			for _ in 0..read_ue(&mut r)? {
				read_se(&mut r)?; // offset_for_ref_frame
			}
		}
		_ => {}
	}
	read_ue(&mut r)?; // max_num_ref_frames
	r.skip_bits(1)?; // gaps_in_frame_num_value_allowed_flag
	read_ue(&mut r)?; // pic_width_in_mbs_minus1
	read_ue(&mut r)?; // pic_height_in_map_units_minus1
	let frame_only = r.read_flag()?; // frame_mbs_only_flag
	if !frame_only {
		r.skip_bits(1)?; // mb_adaptive_frame_field_flag
	}
	r.skip_bits(1)?; // direct_8x8_inference_flag
	if r.read_flag()? {
		for _ in 0..4 {
			read_ue(&mut r)?; // frame_crop_*_offset
		}
	}
	if !r.read_flag()? {
		return Ok(None);
	}

	// vui_parameters()
	if r.read_flag()? && r.read_u8()? == 255 {
		r.skip_bits(32)?; // sar_width, sar_height
	}
	if r.read_flag()? {
		r.skip_bits(1)?; // overscan_appropriate_flag
	}
	if r.read_flag()? {
		r.skip_bits(4)?; // video_format, video_full_range_flag
		if r.read_flag()? {
			r.skip_bits(24)?; // colour_primaries, transfer_characteristics, matrix_coefficients
		}
	}
	if r.read_flag()? {
		read_ue(&mut r)?; // chroma_sample_loc_type_top_field
		read_ue(&mut r)?; // chroma_sample_loc_type_bottom_field
	}
	let mut period = None;
	if r.read_flag()? {
		let units = r.read_bits(32)?;
		let scale = r.read_bits(32)?;
		// With a fixed rate, a frame lasts two ticks (E.2.1).
		if r.read_flag()? && units > 0 && scale > 0 {
			period = Some((2 * u64::from(units), u64::from(scale)));
		}
	}
	let nal_hrd = r.read_flag()?;
	let hrd = match nal_hrd {
		true => Some(read_hrd(&mut r)?),
		false => None,
	};
	let vcl_hrd = r.read_flag()?;
	if vcl_hrd {
		read_hrd(&mut r)?;
	}
	if nal_hrd || vcl_hrd {
		r.skip_bits(1)?; // low_delay_hrd_flag
	}
	r.skip_bits(1)?; // pic_struct_present_flag
	if !r.read_flag()? {
		return Ok(Some(Vui {
			frame_only,
			period,
			depth: None,
			hrd,
		}));
	}
	r.skip_bits(1)?; // motion_vectors_over_pic_boundaries_flag
	for _ in 0..4 {
		read_ue(&mut r)?; // max_bytes_per_pic_denom .. log2_max_mv_length_vertical
	}
	let depth = read_ue(&mut r)?; // max_num_reorder_frames
	Ok(Some(Vui {
		frame_only,
		period,
		depth: Some(depth),
		hrd,
	}))
}

/// Read `hrd_parameters()` (ITU-T H.264 E.1.2) down to its last schedule (E.2.2).
fn read_hrd(r: &mut h264_parser::bitreader::BitReader) -> h264_parser::Result<crate::codec::video::Hrd> {
	use h264_parser::eg::read_ue;

	let cpb_cnt_minus1 = read_ue(r)?;
	if cpb_cnt_minus1 > 31 {
		return Err(h264_parser::Error::MalformedSps("cpb_cnt_minus1 out of range".into()));
	}
	let bit_rate_scale = r.read_bits(4)?;
	let cpb_size_scale = r.read_bits(4)?;
	let mut hrd = crate::codec::video::Hrd {
		bit_rate: 0,
		cpb_size: 0,
	};
	for _ in 0..=cpb_cnt_minus1 {
		let bit_rate = u64::from(read_ue(r)?) + 1;
		let cpb_size = u64::from(read_ue(r)?) + 1;
		r.skip_bits(1)?; // cbr_flag
		hrd = crate::codec::video::Hrd {
			bit_rate: bit_rate << (6 + bit_rate_scale),
			cpb_size: cpb_size << (4 + cpb_size_scale),
		};
	}
	r.skip_bits(20)?; // four delay/offset lengths
	Ok(hrd)
}

/// Parsed AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2).
///
/// Just the codec-config fields that the hang catalog records. The original
/// avcC bytes are still what gets stored as the catalog `description`; this
/// struct is for the field extraction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Avcc {
	/// AVC profile indication (`profile_idc`) from the record.
	pub profile: u8,
	/// Packed constraint-set flags byte from the record.
	pub constraints: u8,
	/// AVC level indication (`level_idc`) from the record.
	pub level: u8,
	/// NALU length size in bytes (typically 4).
	pub length_size: usize,
	/// SPS NAL units carried out-of-band in the record.
	pub sps: Vec<Bytes>,
	/// PPS NAL units carried out-of-band in the record.
	pub pps: Vec<Bytes>,
	/// Resolution from the embedded SPS, if one was present and parseable.
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
}

impl Avcc {
	/// Parse an AVCDecoderConfigurationRecord buffer.
	pub fn parse(avcc: &[u8]) -> Result<Self> {
		if avcc.len() < 7 {
			return Err(Error::AvccTooShort);
		}

		let profile = avcc[1];
		let constraints = avcc[2];
		let level = avcc[3];
		let length_size = (avcc[4] & 0x03) as usize + 1;
		let num_sps = (avcc[5] & 0x1f) as usize;

		let mut pos = 6;
		let sps = read_param_sets(avcc, &mut pos, num_sps)?;

		if avcc.len() <= pos {
			return Err(Error::AvccTruncated);
		}
		let num_pps = avcc[pos] as usize;
		pos += 1;
		let pps = read_param_sets(avcc, &mut pos, num_pps)?;

		// Resolution from the first parseable SPS.
		let (mut coded_width, mut coded_height) = (None, None);
		if let Some(first) = sps.first()
			&& first.len() > 1
			&& let Ok(parsed) = Sps::parse(first)
		{
			coded_width = Some(parsed.coded_width);
			coded_height = Some(parsed.coded_height);
		}

		Ok(Self {
			profile,
			constraints,
			level,
			length_size,
			sps,
			pps,
			coded_width,
			coded_height,
		})
	}
}

fn pack_constraint_flags(sps: &h264_parser::Sps) -> u8 {
	((sps.constraint_set0_flag as u8) << 7)
		| ((sps.constraint_set1_flag as u8) << 6)
		| ((sps.constraint_set2_flag as u8) << 5)
		| ((sps.constraint_set3_flag as u8) << 4)
		| ((sps.constraint_set4_flag as u8) << 3)
		| ((sps.constraint_set5_flag as u8) << 2)
}

/// Build an AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2) from the
/// given SPS and PPS NALs. At least one SPS is required; the profile/level fields
/// are read from the first SPS. A stream may legitimately carry several distinct
/// SPS/PPS (slices reference them by id), so the record holds an ordered list of
/// each rather than a single one.
pub(crate) fn build_avcc(sps_nals: &[Bytes], pps_nals: &[Bytes]) -> Result<Bytes> {
	let first_sps = sps_nals.first().ok_or(Error::MissingSps)?;
	if first_sps.len() < 4 {
		return Err(Error::SpsTooShort);
	}
	// numOfSequenceParameterSets is a 5-bit field, numOfPictureParameterSets a byte.
	if sps_nals.len() > 0x1f {
		return Err(Error::TooManySps(sps_nals.len()));
	}
	if pps_nals.len() > u8::MAX as usize {
		return Err(Error::TooManyPps(pps_nals.len()));
	}
	for sps in sps_nals {
		if sps.len() > u16::MAX as usize {
			return Err(Error::SpsTooLarge(sps.len()));
		}
	}
	for pps in pps_nals {
		if pps.len() > u16::MAX as usize {
			return Err(Error::PpsTooLarge(pps.len()));
		}
	}

	let profile_idc = first_sps[1];
	let constraints = first_sps[2];
	let level_idc = first_sps[3];

	let payload: usize = sps_nals.iter().chain(pps_nals).map(|n| 2 + n.len()).sum();
	let mut out = BytesMut::with_capacity(7 + payload);
	out.put_u8(1); // configurationVersion
	out.put_u8(profile_idc);
	out.put_u8(constraints);
	out.put_u8(level_idc);
	out.put_u8(0xff); // reserved (6 bits) | lengthSizeMinusOne (2 bits = 3)
	out.put_u8(0xe0 | sps_nals.len() as u8); // reserved (3 bits) | numOfSequenceParameterSets
	for sps in sps_nals {
		out.put_u16(sps.len() as u16);
		out.put_slice(sps);
	}
	out.put_u8(pps_nals.len() as u8); // numOfPictureParameterSets
	for pps in pps_nals {
		out.put_u16(pps.len() as u16);
		out.put_slice(pps);
	}
	Ok(out.freeze())
}

/// An avcC whose parameter sets will arrive in the samples, from a catalog codec
/// string that already fixes the record.
///
/// Baseline, Main, and Extended carry no avcC extension. High is 4:2:0 8-bit, which
/// the extension must state when the SPS is absent. Every other profile leaves chroma
/// format or bit depth to the SPS, so the record has to wait for one.
pub(crate) fn catalog_avcc(h264: &hang::catalog::H264) -> Option<Bytes> {
	if !h264.inline {
		return None;
	}
	let extension = match h264.profile {
		66 | 77 | 88 => false,
		100 => true,
		_ => return None,
	};

	let mut out = BytesMut::with_capacity(if extension { 11 } else { 7 });
	out.put_u8(1);
	out.put_u8(h264.profile);
	out.put_u8(h264.constraints);
	out.put_u8(h264.level);
	out.put_u8(0xff); // lengthSizeMinusOne = 3
	out.put_u8(0xe0); // numOfSequenceParameterSets = 0
	out.put_u8(0); // numOfPictureParameterSets = 0
	if extension {
		out.put_u8(0xfc | 1); // chroma_format_idc = 1 (4:2:0)
		out.put_u8(0xf8); // bit_depth_luma_minus8 = 0
		out.put_u8(0xf8); // bit_depth_chroma_minus8 = 0
		out.put_u8(0); // numOfSequenceParameterSetExt = 0
	}
	Some(out.freeze())
}

/// Read `count` length-prefixed (u16) NAL units from `buf` starting at `*pos`,
/// advancing `*pos` past the last one. All arithmetic is checked so malformed
/// configs surface as errors rather than panics.
fn read_param_sets(buf: &[u8], pos: &mut usize, count: usize) -> Result<Vec<Bytes>> {
	let mut out = Vec::with_capacity(count);
	for _ in 0..count {
		let after_len = pos.checked_add(2).ok_or(Error::AvccTruncated)?;
		if buf.len() < after_len {
			return Err(Error::AvccTruncated);
		}
		let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
		let after_nal = after_len.checked_add(len).ok_or(Error::AvccTruncated)?;
		if buf.len() < after_nal {
			return Err(Error::AvccTruncated);
		}
		out.push(Bytes::copy_from_slice(&buf[after_len..after_nal]));
		*pos = after_nal;
	}
	Ok(out)
}

/// Extract the parameter-set NALs (SPS then PPS) and the NALU length size from
/// an AVCDecoderConfigurationRecord. The inverse of [`build_avcc`]; used to
/// re-emit out-of-band avc1 parameter sets as inline Annex-B (e.g. for MPEG-TS).
pub(crate) fn avcc_params(avcc: &[u8]) -> anyhow::Result<(usize, Vec<Bytes>)> {
	anyhow::ensure!(avcc.len() >= 6, "AVCDecoderConfigurationRecord too short");
	let length_size = (avcc[4] & 0x03) as usize + 1;

	let mut params = Vec::new();
	let num_sps = avcc[5] & 0x1f;
	let mut pos = read_param_set_array(avcc, 6, num_sps as usize, &mut params)?;

	anyhow::ensure!(avcc.len() > pos, "avcC missing PPS count");
	let num_pps = avcc[pos];
	pos += 1;
	read_param_set_array(avcc, pos, num_pps as usize, &mut params)?;

	Ok((length_size, params))
}

/// Read `count` u16-length-prefixed NALs starting at `pos`, appending each to
/// `params`. Returns the offset just past the last NAL read.
fn read_param_set_array(buf: &[u8], mut pos: usize, count: usize, params: &mut Vec<Bytes>) -> anyhow::Result<usize> {
	for _ in 0..count {
		anyhow::ensure!(buf.len() >= pos + 2, "truncated parameter-set length");
		let len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
		pos += 2;
		anyhow::ensure!(buf.len() >= pos + len, "parameter-set NAL exceeds buffer");
		params.push(Bytes::copy_from_slice(&buf[pos..pos + len]));
		pos += len;
	}
	Ok(pos)
}

/// Transform H.264 frames from Annex-B (inline SPS/PPS, "avc3") to
/// length-prefixed NALU (out-of-band AVCDecoderConfigurationRecord, "avc1").
///
/// The avcC is synthesized from the active SPS+PPS and exposed via
/// [`Self::avcc`]. Once it returns `Some`, all subsequent calls to
/// [`Self::transform`] return length-prefixed sample data suitable for an avc1
/// container (e.g. MKV `V_MPEG4/ISO/AVC` with the avcC in CodecPrivate).
///
/// The active set is scoped to the latest keyframe: a frame that carries
/// parameter sets redefines them, so a mid-stream reconfiguration drops the
/// superseded SPS/PPS instead of accumulating them forever.
pub struct Avc1 {
	avcc: Option<Bytes>,
	/// Keep SPS and PPS in the sample instead of lifting them into an avcC.
	in_band: bool,
	/// The active SPS NALs (from the most recent keyframe that carried them).
	sps: Vec<Bytes>,
	/// The active PPS NALs.
	pps: Vec<Bytes>,
}

impl Default for Avc1 {
	fn default() -> Self {
		Self::new()
	}
}

impl Avc1 {
	/// Build a new transform for an avc3 source.
	pub fn new() -> Self {
		Self {
			avcc: None,
			in_band: false,
			sps: Vec::new(),
			pps: Vec::new(),
		}
	}

	/// Length-prefix every NAL, parameter sets included, and re-inject a cached
	/// set on a keyframe that omits one. No avcC is built: the caller already has
	/// the record.
	pub(crate) fn keeping_parameter_sets() -> Self {
		Self {
			in_band: true,
			..Self::new()
		}
	}

	/// The AVCDecoderConfigurationRecord, available once SPS+PPS have been observed.
	pub fn avcc(&self) -> Option<&Bytes> {
		self.avcc.as_ref()
	}

	/// Convert one decoded frame's payload to the avc1 wire shape.
	///
	/// Returns:
	/// - `Ok(Some(payload))` if a length-prefixed sample is ready to emit.
	/// - `Ok(None)` if the input contained only parameter sets and the
	///   transform is still waiting for slice NALs (avcC may have been built
	///   as a side effect).
	pub fn transform(&mut self, payload: Bytes) -> Result<Option<Bytes>> {
		self.transform_frame(payload, false)
	}

	pub(crate) fn transform_frame(&mut self, payload: Bytes, keyframe: bool) -> Result<Option<Bytes>> {
		if self.in_band {
			return self.transform_in_band(payload, keyframe);
		}

		// Parse Annex-B NALs, collect this frame's SPS/PPS, length-prefix the
		// rest. NalIterator advances the Bytes cursor; the trailing NAL has to be
		// pulled separately via flush().
		let mut buf = payload.clone();
		let mut nal_iter = crate::codec::annexb::NalIterator::new(&mut buf);

		let mut out = BytesMut::with_capacity(payload.remaining());
		let mut frame_sps: Vec<Bytes> = Vec::new();
		let mut frame_pps: Vec<Bytes> = Vec::new();
		let mut emitted_any_slice = false;

		loop {
			let nal = match nal_iter.next() {
				Some(Ok(n)) => n,
				Some(Err(e)) => return Err(e.into()),
				None => break,
			};
			if process_nal(&nal, &mut out, &mut frame_sps, &mut frame_pps)? {
				emitted_any_slice = true;
			}
		}

		if let Some(nal) = nal_iter.flush()?
			&& process_nal(&nal, &mut out, &mut frame_sps, &mut frame_pps)?
		{
			emitted_any_slice = true;
		}

		// A frame that carries parameter sets (a keyframe) redefines the active
		// set; adopt it so SPS/PPS from a superseded configuration are dropped
		// rather than lingering in the avcC. Per type, so a frame that updates only
		// one of SPS/PPS keeps the other.
		let mut changed = false;
		if !frame_sps.is_empty() && frame_sps != self.sps {
			self.sps = frame_sps;
			changed = true;
		}
		if !frame_pps.is_empty() && frame_pps != self.pps {
			self.pps = frame_pps;
			changed = true;
		}
		if changed {
			self.rebuild_avcc()?;
		}

		if !emitted_any_slice {
			return Ok(None);
		}

		Ok(Some(out.freeze()))
	}

	fn rebuild_avcc(&mut self) -> Result<()> {
		if self.sps.is_empty() || self.pps.is_empty() {
			return Ok(());
		}
		self.avcc = Some(build_avcc(&self.sps, &self.pps)?);
		Ok(())
	}

	/// Length-prefix the access unit, parameter sets included. A keyframe that
	/// omitted SPS or PPS gets the cached set of that type immediately before
	/// its first slice, so a receiver tuning in there still has them.
	fn transform_in_band(&mut self, payload: Bytes, keyframe: bool) -> Result<Option<Bytes>> {
		let nals = crate::codec::annexb::nal_units(&payload)?;
		let mut frame_sps = Vec::new();
		let mut frame_pps = Vec::new();
		let mut vcl_at = None;
		let mut has_sample = false;
		for (index, nal) in nals.iter().enumerate() {
			if nal.is_empty() {
				continue;
			}
			match nal[0] & 0x1f {
				NAL_TYPE_SPS => {
					crate::codec::annexb::push_distinct(&mut frame_sps, nal);
				}
				NAL_TYPE_PPS => {
					crate::codec::annexb::push_distinct(&mut frame_pps, nal);
				}
				1..=5 => {
					has_sample = true;
					if vcl_at.is_none() {
						vcl_at = Some(index);
					}
				}
				_ => has_sample = true,
			}
		}

		let had_sps = !frame_sps.is_empty();
		let had_pps = !frame_pps.is_empty();
		if had_sps && frame_sps != self.sps {
			self.sps = frame_sps;
		}
		if had_pps && frame_pps != self.pps {
			self.pps = frame_pps;
		}
		if !has_sample {
			return Ok(None);
		}

		// A keyframe with no slice (SEI only) is injected at the front. A delta
		// frame is emitted as it arrived: missing parameter sets are not invented
		// into the middle of a GOP.
		let split = if keyframe { vcl_at.unwrap_or(0) } else { nals.len() };
		let mut out = BytesMut::new();
		for nal in &nals[..split] {
			length_prefix(&mut out, nal)?;
		}
		if keyframe && !had_sps {
			for nal in &self.sps {
				length_prefix(&mut out, nal)?;
			}
		}
		if keyframe && !had_pps {
			for nal in &self.pps {
				length_prefix(&mut out, nal)?;
			}
		}
		for nal in &nals[split..] {
			length_prefix(&mut out, nal)?;
		}
		Ok(Some(out.freeze()))
	}
}

fn length_prefix(out: &mut BytesMut, nal: &Bytes) -> Result<()> {
	if nal.is_empty() {
		return Ok(());
	}
	let len = u32::try_from(nal.len()).map_err(|_| Error::NalTooLarge)?;
	out.extend_from_slice(&len.to_be_bytes());
	out.extend_from_slice(nal);
	Ok(())
}

/// Process one NAL: SPS/PPS are collected (distinctly) into this frame's sets,
/// everything else is length-prefixed and appended to `out`. Returns true if the
/// NAL was a slice (i.e. produced sample bytes).
fn process_nal(
	nal: &Bytes,
	out: &mut BytesMut,
	frame_sps: &mut Vec<Bytes>,
	frame_pps: &mut Vec<Bytes>,
) -> Result<bool> {
	if nal.is_empty() {
		return Ok(false);
	}
	match nal[0] & 0x1f {
		NAL_TYPE_SPS => {
			crate::codec::annexb::push_distinct(frame_sps, nal);
			Ok(false)
		}
		NAL_TYPE_PPS => {
			crate::codec::annexb::push_distinct(frame_pps, nal);
			Ok(false)
		}
		_ => {
			let len = u32::try_from(nal.len()).map_err(|_| Error::NalTooLarge)?;
			out.extend_from_slice(&len.to_be_bytes());
			out.extend_from_slice(nal);
			Ok(true)
		}
	}
}

/// Real SPS NAL units from 64x64, 25 fps x264 encodes (High profile, 50 Hz VUI tick), for
/// tests that need a VUI with a bitstream restriction.
#[cfg(test)]
pub(crate) mod fixtures {
	/// `bframes=1:force-cfr=1`: one B-frame per reference (`max_num_reorder_frames` 1) with
	/// `fixed_frame_rate_flag` set.
	pub(crate) const SPS_IPB: &[u8] = &[
		0x67, 0x64, 0x00, 0x0a, 0xac, 0xe4, 0x10, 0x9b, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03,
		0x03, 0x28, 0xf1, 0x22, 0x51, 0x20,
	];
	/// `bframes=3:b-pyramid=2:force-cfr=1`: a B-pyramid (`max_num_reorder_frames` 2) with
	/// `fixed_frame_rate_flag` set.
	pub(crate) const SPS_PYRAMID: &[u8] = &[
		0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x44, 0x26, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03,
		0x00, 0xca, 0x3c, 0x48, 0x96, 0x58,
	];
	/// `bframes=1` at x264's default: `max_num_reorder_frames` 1 without `fixed_frame_rate_flag`.
	pub(crate) const SPS_IPB_VARIABLE: &[u8] = &[
		0x67, 0x64, 0x00, 0x0a, 0xac, 0xe4, 0x10, 0x9b, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03,
		0x03, 0x20, 0xf1, 0x22, 0x51, 0x20,
	];
	/// `interlaced=1:tff=1:bframes=0` at 320x240, with `fixed_frame_rate_flag` forced on: the same
	/// 50 Hz tick, but `frame_mbs_only_flag` 0 leaves `pic_struct` to the slice header.
	pub(crate) const SPS_FIELD: &[u8] = &[
		0x67, 0xf4, 0x00, 0x15, 0x91, 0x9d, 0x02, 0x82, 0x1f, 0x89, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00,
		0x00, 0x03, 0x00, 0xca, 0x7c, 0x50, 0xaa, 0x80,
	];
	pub(crate) const PPS: &[u8] = &[0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];
}

#[cfg(test)]
mod tests {
	use super::*;

	const SC4: &[u8] = &[0, 0, 0, 1];

	#[test]
	fn sps_reorder_reads_the_vui() {
		use crate::codec::video::Reorder;

		let fixed = Some((2, 50));
		assert_eq!(
			sps_reorder(fixtures::SPS_IPB),
			Some(Reorder {
				depth: 1,
				period: fixed
			})
		);
		assert_eq!(
			sps_reorder(fixtures::SPS_PYRAMID),
			Some(Reorder {
				depth: 2,
				period: fixed
			})
		);
		assert_eq!(
			sps_reorder(fixtures::SPS_IPB_VARIABLE),
			Some(Reorder { depth: 1, period: None })
		);
		// The field-capable SPS still declares its depth and tick; only the frame rate needs
		// `pic_struct` to read it.
		assert_eq!(
			sps_reorder(fixtures::SPS_FIELD),
			Some(Reorder {
				depth: 0,
				period: Some((2, 50))
			})
		);
		// Baseline with no VUI declares nothing.
		assert_eq!(sps_reorder(&[0x67, 0x42, 0xc0, 0x1f, 0xde]), None);
		// A VUI cut short is no declaration rather than a wrong one.
		assert_eq!(sps_reorder(&fixtures::SPS_IPB[..fixtures::SPS_IPB.len() - 3]), None);
	}

	#[test]
	fn sps_framerate_needs_a_fixed_rate() {
		assert_eq!(sps_framerate(fixtures::SPS_IPB), Some(25.0));
		// The same 50 Hz tick without `fixed_frame_rate_flag` is only a ceiling.
		assert_eq!(sps_framerate(fixtures::SPS_IPB_VARIABLE), None);
		// A field-capable SPS makes the tick a field or a frame depending on `pic_struct`.
		assert_eq!(sps_framerate(fixtures::SPS_FIELD), None);
		assert_eq!(sps_framerate(&[0x67, 0x42, 0xc0, 0x1f, 0xde]), None);
	}

	fn annexb_frame(nals: &[&[u8]]) -> Bytes {
		let mut buf = BytesMut::new();
		for nal in nals {
			buf.extend_from_slice(SC4);
			buf.extend_from_slice(nal);
		}
		buf.freeze()
	}

	/// avc1: a length-prefixed access unit with an IDR slice wraps as one keyframe;
	/// the payload is passed through verbatim.
	#[test]
	fn avc1_frame_keyframe() {
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let mut au = BytesMut::new();
		au.extend_from_slice(&(idr.len() as u32).to_be_bytes());
		au.extend_from_slice(idr);

		let frame = avc1_frame(&au, 4, moq_net::Timestamp::from_micros(0).unwrap()).unwrap();
		assert!(frame.keyframe);
		assert_eq!(frame.payload[4..], *idr);
	}

	/// avc1: a length-prefixed access unit with a non-IDR slice is a delta frame.
	#[test]
	fn avc1_frame_delta() {
		let pslice: &[u8] = &[0x61, 0xe0, 0x12, 0x34];
		let mut au = BytesMut::new();
		au.extend_from_slice(&(pslice.len() as u32).to_be_bytes());
		au.extend_from_slice(pslice);

		let frame = avc1_frame(&au, 4, moq_net::Timestamp::from_micros(0).unwrap()).unwrap();
		assert!(!frame.keyframe);
	}

	#[test]
	fn avc3_strips_sps_pps_and_builds_avcc() {
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];
		let idr = &[0x65, 0x88, 0x84, 0x21][..];

		let mut tx = Avc1::new();
		assert!(tx.avcc().is_none());

		let frame = annexb_frame(&[sps, pps, idr]);
		let out = tx.transform(frame).expect("transform").expect("expected output");

		let avcc = tx.avcc().expect("avcC available").clone();
		assert_eq!(avcc[0], 1);
		assert_eq!(avcc[1], sps[1]);
		assert_eq!(avcc[3], sps[3]);

		let mut expected = BytesMut::new();
		expected.extend_from_slice(&(idr.len() as u32).to_be_bytes());
		expected.extend_from_slice(idr);
		assert_eq!(out.as_ref(), expected.as_ref());
	}

	#[test]
	fn avcc_params_roundtrips_build_avcc() {
		let sps = Bytes::from_static(&[0x67, 0x42, 0xc0, 0x1f, 0xde]);
		let pps = Bytes::from_static(&[0x68, 0xce, 0x3c, 0x80]);

		let avcc = build_avcc(std::slice::from_ref(&sps), std::slice::from_ref(&pps)).unwrap();
		let (length_size, params) = avcc_params(&avcc).unwrap();

		assert_eq!(length_size, 4);
		assert_eq!(params.len(), 2);
		assert_eq!(params[0], sps);
		assert_eq!(params[1], pps);
	}

	#[test]
	fn build_avcc_carries_multiple_pps() {
		// A source with one SPS and two PPS (ids 0 and 1): the avcC must keep both,
		// in order, so slices referencing either id stay decodable.
		let sps = Bytes::from_static(&[0x67, 0x42, 0xc0, 0x1f, 0xde]);
		let pps0 = Bytes::from_static(&[0x68, 0xce, 0x3c, 0x80]);
		let pps1 = Bytes::from_static(&[0x68, 0xce, 0x3c, 0x81]);

		let avcc = build_avcc(std::slice::from_ref(&sps), &[pps0.clone(), pps1.clone()]).unwrap();
		// numOfSequenceParameterSets is the low 5 bits of byte 5.
		assert_eq!(avcc[5] & 0x1f, 1);

		let (_, params) = avcc_params(&avcc).unwrap();
		assert_eq!(params, vec![sps, pps0, pps1]);
	}

	#[test]
	fn avc3_keyframe_with_two_pps_keeps_both() {
		// One keyframe carrying both PPS: the synthesized avcC keeps both, in order.
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps0 = &[0x68, 0xce, 0x3c, 0x80][..];
		let pps1 = &[0x68, 0xce, 0x3c, 0x81][..];
		let idr = &[0x65, 0x88][..];

		let mut tx = Avc1::new();
		tx.transform(annexb_frame(&[sps, pps0, pps1, idr])).unwrap();

		let avcc = tx.avcc().expect("avcC available");
		let (_, params) = avcc_params(avcc).unwrap();
		assert_eq!(
			params.iter().map(|p| p.as_ref()).collect::<Vec<_>>(),
			vec![sps, pps0, pps1]
		);
	}

	#[test]
	fn avc3_reinit_drops_superseded_pps() {
		// A later keyframe presents a different PPS set: the avcC adopts the new set
		// and drops the old one rather than accumulating both forever.
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps0 = &[0x68, 0xce, 0x3c, 0x80][..];
		let pps1 = &[0x68, 0xce, 0x3c, 0x81][..];
		let idr = &[0x65, 0x88][..];

		let mut tx = Avc1::new();
		tx.transform(annexb_frame(&[sps, pps0, idr])).unwrap();
		tx.transform(annexb_frame(&[sps, pps1, idr])).unwrap();

		let avcc = tx.avcc().expect("avcC available");
		let (_, params) = avcc_params(avcc).unwrap();
		assert_eq!(
			params.iter().map(|p| p.as_ref()).collect::<Vec<_>>(),
			vec![sps, pps1],
			"reinit must drop the superseded PPS"
		);
	}

	#[test]
	fn avc3_parameter_only_frame_returns_none() {
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];

		let mut tx = Avc1::new();
		let frame = annexb_frame(&[sps, pps]);
		assert!(tx.transform(frame).unwrap().is_none());
		assert!(tx.avcc().is_some());
	}

	#[test]
	fn avc3_subsequent_frame_uses_cached_avcc() {
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];
		let idr = &[0x65, 0x88][..];
		let p = &[0x61, 0xe0, 0x12][..];

		let mut tx = Avc1::new();
		tx.transform(annexb_frame(&[sps, pps, idr])).unwrap();
		let avcc_v1 = tx.avcc().unwrap().clone();

		let out = tx.transform(annexb_frame(&[p])).unwrap().unwrap();
		assert_eq!(tx.avcc().unwrap(), &avcc_v1);
		let mut expected = BytesMut::new();
		expected.extend_from_slice(&(p.len() as u32).to_be_bytes());
		expected.extend_from_slice(p);
		assert_eq!(out.as_ref(), expected.as_ref());
	}

	#[test]
	fn avc3_export_e2e_payload_shape() {
		// Mirror the byte shapes used by the export integration test so any
		// divergence surfaces here in isolation.
		let sps = &[0x67u8, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef][..];
		let pps = &[0x68u8, 0xce, 0x3c, 0x80][..];
		let idr = &[0x65u8, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33][..];
		let pslice = &[0x61u8, 0xe0, 0x12, 0x34][..];

		let mut tx = Avc1::new();
		let key = annexb_frame(&[sps, pps, idr]);
		let key_out = tx.transform(key).expect("transform key").expect("output");
		assert!(tx.avcc().is_some());

		assert_eq!(key_out.len(), 4 + idr.len());
		assert_eq!(&key_out[4..], idr);

		let p = annexb_frame(&[pslice]);
		let p_out = tx.transform(p).expect("transform p").expect("output");
		assert_eq!(p_out.len(), 4 + pslice.len());
		assert_eq!(&p_out[4..], pslice);
	}

	fn length_prefixed(nals: &[&[u8]]) -> Vec<u8> {
		let mut out = Vec::new();
		for nal in nals {
			out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			out.extend_from_slice(nal);
		}
		out
	}

	fn decode_avcc(bytes: &Bytes) -> mp4_atom::Avcc {
		use mp4_atom::Atom;
		mp4_atom::Avcc::decode_body(&mut std::io::Cursor::new(bytes.as_ref())).unwrap()
	}

	#[test]
	fn catalog_avcc_baseline_has_no_extension() {
		let h264 = hang::catalog::H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		};
		let avcc = decode_avcc(&catalog_avcc(&h264).unwrap());
		assert_eq!(avcc.avc_profile_indication, 0x42);
		assert_eq!(avcc.profile_compatibility, 0xc0);
		assert_eq!(avcc.avc_level_indication, 0x1f);
		assert_eq!(avcc.length_size, 4);
		assert!(avcc.sequence_parameter_sets.is_empty());
		assert!(avcc.picture_parameter_sets.is_empty());
		assert!(avcc.ext.is_none());
	}

	#[test]
	fn catalog_avcc_high_states_420_8bit() {
		let h264 = hang::catalog::H264 {
			profile: 100,
			constraints: 0,
			level: 0x28,
			inline: true,
		};
		let avcc = decode_avcc(&catalog_avcc(&h264).unwrap());
		assert_eq!(avcc.avc_profile_indication, 100);
		assert!(avcc.sequence_parameter_sets.is_empty());
		assert_eq!(
			avcc.ext,
			Some(mp4_atom::AvccExt {
				chroma_format: 1,
				bit_depth_luma: 8,
				bit_depth_chroma: 8,
				sequence_parameter_sets_ext: Vec::new(),
			})
		);
	}

	#[test]
	fn catalog_avcc_refuses_profiles_the_string_cannot_describe() {
		for profile in [110, 122, 244, 44, 144] {
			let h264 = hang::catalog::H264 {
				profile,
				constraints: 0,
				level: 0x1f,
				inline: true,
			};
			assert!(catalog_avcc(&h264).is_none(), "profile {profile}");
		}
		let out_of_band = hang::catalog::H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: false,
		};
		assert!(catalog_avcc(&out_of_band).is_none());
	}

	#[test]
	fn in_band_keeps_parameter_sets_and_reinjects_them() {
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];
		let idr = &[0x65, 0x88, 0x84, 0x21][..];
		let sei = &[0x06, 0x05, 0xff][..];
		let sps2 = &[0x67, 0x42, 0xc0, 0x1f, 0xaa][..];
		let delta = &[0x61, 0xe0, 0x12][..];

		let mut tx = Avc1::keeping_parameter_sets();
		let kept = tx
			.transform_frame(annexb_frame(&[sps, pps, idr]), true)
			.unwrap()
			.unwrap();
		assert_eq!(kept.as_ref(), length_prefixed(&[sps, pps, idr]));
		assert!(tx.avcc().is_none());

		let mut tx = Avc1::keeping_parameter_sets();
		assert!(tx.transform_frame(annexb_frame(&[sps, pps]), true).unwrap().is_none());
		assert!(tx.avcc().is_none());
		let bare = tx.transform_frame(annexb_frame(&[idr]), true).unwrap().unwrap();
		assert_eq!(bare.as_ref(), length_prefixed(&[sps, pps, idr]));

		let replaced = tx.transform_frame(annexb_frame(&[sps2, idr]), true).unwrap().unwrap();
		assert_eq!(replaced.as_ref(), length_prefixed(&[sps2, pps, idr]));

		let prefixed = tx.transform_frame(annexb_frame(&[sei, idr]), true).unwrap().unwrap();
		assert_eq!(prefixed.as_ref(), length_prefixed(&[sei, sps2, pps, idr]));

		let delta_out = tx.transform_frame(annexb_frame(&[delta]), false).unwrap().unwrap();
		assert_eq!(delta_out.as_ref(), length_prefixed(&[delta]));
	}
}
