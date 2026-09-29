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
	if nal.len() < 4 {
		return None;
	}
	let rbsp = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
	let sps = h264_parser::Sps::parse(&rbsp).ok()?;
	if !sps.vui_parameters_present_flag {
		return None;
	}
	vui_reorder(&rbsp).ok().flatten()
}

/// Walk an SPS RBSP to its VUI (ITU-T H.264 7.3.2.1.1, E.1.1). `h264_parser` stops at
/// `vui_parameters_present_flag` without exposing its position, so this re-reads the fields
/// before it.
fn vui_reorder(rbsp: &[u8]) -> h264_parser::Result<Option<crate::codec::video::Reorder>> {
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
	if !r.read_flag()? {
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
	if nal_hrd {
		skip_hrd(&mut r)?;
	}
	let vcl_hrd = r.read_flag()?;
	if vcl_hrd {
		skip_hrd(&mut r)?;
	}
	if nal_hrd || vcl_hrd {
		r.skip_bits(1)?; // low_delay_hrd_flag
	}
	r.skip_bits(1)?; // pic_struct_present_flag
	if !r.read_flag()? {
		return Ok(None);
	}
	r.skip_bits(1)?; // motion_vectors_over_pic_boundaries_flag
	for _ in 0..4 {
		read_ue(&mut r)?; // max_bytes_per_pic_denom .. log2_max_mv_length_vertical
	}
	let depth = read_ue(&mut r)?; // max_num_reorder_frames
	Ok(Some(crate::codec::video::Reorder { depth, period }))
}

/// Skip `hrd_parameters()` (ITU-T H.264 E.1.2).
fn skip_hrd(r: &mut h264_parser::bitreader::BitReader) -> h264_parser::Result<()> {
	use h264_parser::eg::read_ue;

	let cpb_cnt_minus1 = read_ue(r)?;
	if cpb_cnt_minus1 > 31 {
		return Err(h264_parser::Error::MalformedSps("cpb_cnt_minus1 out of range".into()));
	}
	r.skip_bits(8)?; // bit_rate_scale, cpb_size_scale
	for _ in 0..=cpb_cnt_minus1 {
		read_ue(r)?; // bit_rate_value_minus1
		read_ue(r)?; // cpb_size_value_minus1
		r.skip_bits(1)?; // cbr_flag
	}
	r.skip_bits(20) // four delay/offset lengths
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
			sps: Vec::new(),
			pps: Vec::new(),
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
		// Baseline with no VUI declares nothing.
		assert_eq!(sps_reorder(&[0x67, 0x42, 0xc0, 0x1f, 0xde]), None);
		// A VUI cut short is no declaration rather than a wrong one.
		assert_eq!(sps_reorder(&fixtures::SPS_IPB[..fixtures::SPS_IPB.len() - 3]), None);
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
}
