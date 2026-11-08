// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An Annex B h.265 parser.
//!
//! Parses VPSs, SPSs, PPSs and Slices from NALUs.

use std::collections::BTreeMap;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::rc::Rc;

use crate::bitstream_utils::BitReader;
use crate::codec::h264::nalu;
use crate::codec::h264::nalu::Header;
use crate::codec::h264::parser::Point;
use crate::codec::h264::parser::Rect;

// Given the max VPS id.
const MAX_VPS_COUNT: usize = 16;
// Given the max SPS id.
const MAX_SPS_COUNT: usize = 16;
// Given the max PPS id.
const MAX_PPS_COUNT: usize = 64;
// The most tile columns and rows any level allows (Table A.8), including the
// last of each, which is inferred rather than signalled.
const MAX_TILE_COLUMNS: usize = 20;
const MAX_TILE_ROWS: usize = 22;
// 7.4.7.1
const MAX_REF_IDX_ACTIVE: u32 = 15;

// 7.4.3.2.1:
// num_short_term_ref_pic_sets specifies the number of st_ref_pic_set( ) syntax
// structures included in the SPS. The value of num_short_term_ref_pic_sets
// shall be in the range of 0 to 64, inclusive.
// NOTE 5 – A decoder should allocate memory for a total number of
// num_short_term_ref_pic_sets + 1 st_ref_pic_set( ) syntax structures since
// there may be a st_ref_pic_set( ) syntax structure directly signalled in the
// slice headers of a current picture. A st_ref_pic_set( ) syntax structure
// directly signalled in the slice headers of a current picture has an index
// equal to num_short_term_ref_pic_sets.
const MAX_SHORT_TERM_REF_PIC_SETS: usize = 65;

// 7.4.3.2.1:
const MAX_LONG_TERM_REF_PIC_SETS: usize = 32;

// From table 7-5.
const DEFAULT_SCALING_LIST_0: [u8; 16] = [16; 16];

// From Table 7-6.
const DEFAULT_SCALING_LIST_1: [u8; 64] = [
	16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20, 21, 20, 19, 21, 24,
	22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31, 29, 36, 41, 44, 41, 36, 47, 54, 54, 47,
	65, 70, 65, 88, 88, 115,
];

// From Table 7-6.
const DEFAULT_SCALING_LIST_2: [u8; 64] = [
	16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20, 20, 20, 20, 20, 24,
	24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28, 28, 33, 33, 33, 33, 33, 41, 41, 41, 41,
	54, 54, 54, 71, 71, 91,
];

/// Table 7-1 – NAL unit type codes and NAL unit type classes
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum NaluType {
	#[default]
	TrailN = 0,
	TrailR = 1,
	TsaN = 2,
	TsaR = 3,
	StsaN = 4,
	StsaR = 5,
	RadlN = 6,
	RadlR = 7,
	RaslN = 8,
	RaslR = 9,
	RsvVclN10 = 10,
	RsvVclR11 = 11,
	RsvVclN12 = 12,
	RsvVclR13 = 13,
	RsvVclN14 = 14,
	RsvVclR15 = 15,
	BlaWLp = 16,
	BlaWRadl = 17,
	BlaNLp = 18,
	IdrWRadl = 19,
	IdrNLp = 20,
	CraNut = 21,
	RsvIrapVcl22 = 22,
	RsvIrapVcl23 = 23,
	RsvVcl24 = 24,
	RsvVcl25 = 25,
	RsvVcl26 = 26,
	RsvVcl27 = 27,
	RsvVcl28 = 28,
	RsvVcl29 = 29,
	RsvVcl30 = 30,
	RsvVcl31 = 31,
	VpsNut = 32,
	SpsNut = 33,
	PpsNut = 34,
	AudNut = 35,
	EosNut = 36,
	EobNut = 37,
	FdNut = 38,
	PrefixSeiNut = 39,
	SuffixSeiNut = 40,
	RsvNvcl41 = 41,
	RsvNvcl42 = 42,
	RsvNvcl43 = 43,
	RsvNvcl44 = 44,
	RsvNvcl45 = 45,
	RsvNvcl46 = 46,
	RsvNvcl47 = 47,
}

impl TryFrom<u32> for NaluType {
	type Error = String;

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		match value {
			0 => Ok(NaluType::TrailN),
			1 => Ok(NaluType::TrailR),
			2 => Ok(NaluType::TsaN),
			3 => Ok(NaluType::TsaR),
			4 => Ok(NaluType::StsaN),
			5 => Ok(NaluType::StsaR),
			6 => Ok(NaluType::RadlN),
			7 => Ok(NaluType::RadlR),
			8 => Ok(NaluType::RaslN),
			9 => Ok(NaluType::RaslR),
			10 => Ok(NaluType::RsvVclN10),
			11 => Ok(NaluType::RsvVclR11),
			12 => Ok(NaluType::RsvVclN12),
			13 => Ok(NaluType::RsvVclR13),
			14 => Ok(NaluType::RsvVclN14),
			15 => Ok(NaluType::RsvVclR15),
			16 => Ok(NaluType::BlaWLp),
			17 => Ok(NaluType::BlaWRadl),
			18 => Ok(NaluType::BlaNLp),
			19 => Ok(NaluType::IdrWRadl),
			20 => Ok(NaluType::IdrNLp),
			21 => Ok(NaluType::CraNut),
			22 => Ok(NaluType::RsvIrapVcl22),
			23 => Ok(NaluType::RsvIrapVcl23),
			24 => Ok(NaluType::RsvVcl24),
			25 => Ok(NaluType::RsvVcl25),
			26 => Ok(NaluType::RsvVcl26),
			27 => Ok(NaluType::RsvVcl27),
			28 => Ok(NaluType::RsvVcl28),
			29 => Ok(NaluType::RsvVcl29),
			30 => Ok(NaluType::RsvVcl30),
			31 => Ok(NaluType::RsvVcl31),
			32 => Ok(NaluType::VpsNut),
			33 => Ok(NaluType::SpsNut),
			34 => Ok(NaluType::PpsNut),
			35 => Ok(NaluType::AudNut),
			36 => Ok(NaluType::EosNut),
			37 => Ok(NaluType::EobNut),
			38 => Ok(NaluType::FdNut),
			39 => Ok(NaluType::PrefixSeiNut),
			40 => Ok(NaluType::SuffixSeiNut),
			41 => Ok(NaluType::RsvNvcl41),
			42 => Ok(NaluType::RsvNvcl42),
			43 => Ok(NaluType::RsvNvcl43),
			44 => Ok(NaluType::RsvNvcl44),
			45 => Ok(NaluType::RsvNvcl45),
			46 => Ok(NaluType::RsvNvcl46),
			47 => Ok(NaluType::RsvNvcl47),
			_ => Err(format!("Invalid NaluType {}", value)),
		}
	}
}

impl NaluType {
	/// Whether this is an IDR NALU.
	pub fn is_idr(&self) -> bool {
		matches!(self, Self::IdrWRadl | Self::IdrNLp)
	}

	/// Whether this is an IRAP NALU.
	pub fn is_irap(&self) -> bool {
		let type_ = *self as u32;
		type_ >= Self::BlaWLp as u32 && type_ <= Self::RsvIrapVcl23 as u32
	}

	/// Whether this is a BLA NALU.
	pub fn is_bla(&self) -> bool {
		let type_ = *self as u32;
		type_ >= Self::BlaWLp as u32 && type_ <= Self::BlaNLp as u32
	}

	/// Whether this is a CRA NALU.
	pub fn is_cra(&self) -> bool {
		matches!(self, Self::CraNut)
	}

	/// Whether this is a RADL NALU.
	pub fn is_radl(&self) -> bool {
		matches!(self, Self::RadlN | Self::RadlR)
	}

	/// Whether this is a RASL NALU.
	pub fn is_rasl(&self) -> bool {
		matches!(self, Self::RaslN | Self::RaslR)
	}

	//// Whether this is a SLNR NALU.
	pub fn is_slnr(&self) -> bool {
		// From the specification:
		// If a picture has nal_unit_type equal to TRAIL_N, TSA_N, STSA_N,
		// RADL_N, RASL_N, RSV_VCL_N10, RSV_VCL_N12 or RSV_VCL_N14, the picture
		// is an SLNR picture. Otherwise, the picture is a sub-layer reference
		// picture.
		matches!(
			self,
			Self::TrailN
				| Self::TsaN | Self::StsaN
				| Self::RadlN
				| Self::RaslN
				| Self::RsvVclN10
				| Self::RsvVclN12
				| Self::RsvVclN14
		)
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NaluHeader {
	/// The NALU type.
	pub type_: NaluType,
	/// Specifies the identifier of the layer to which a VCL NAL unit belongs or
	/// the identifier of a layer to which a non-VCL NAL unit applies.
	pub nuh_layer_id: u8,
	/// Minus 1 specifies a temporal identifier for the NAL unit. The value of
	/// nuh_temporal_id_plus1 shall not be equal to 0.
	pub nuh_temporal_id_plus1: u8,
}

impl NaluHeader {
	pub fn nuh_temporal_id(&self) -> u8 {
		self.nuh_temporal_id_plus1.saturating_sub(1)
	}
}

impl Header for NaluHeader {
	fn parse<T: AsRef<[u8]>>(cursor: &mut std::io::Cursor<T>) -> Result<Self, String> {
		let mut data = [0u8; 2];
		cursor.read_exact(&mut data).map_err(|_| String::from("Broken Data"))?;
		let mut r = BitReader::new(&data, false);
		let _ = cursor.seek(SeekFrom::Current(-1 * data.len() as i64));

		// Skip forbidden_zero_bit
		r.skip_bits(1)?;

		Ok(Self {
			type_: NaluType::try_from(r.read_bits::<u32>(6)?)?,
			nuh_layer_id: r.read_bits::<u8>(6)?,
			nuh_temporal_id_plus1: r.read_bits::<u8>(3)?,
		})
	}

	fn is_end(&self) -> bool {
		matches!(self.type_, NaluType::EosNut | NaluType::EobNut)
	}

	fn len(&self) -> usize {
		// 7.3.1.2
		2
	}
}

pub type Nalu<'a> = nalu::Nalu<'a, NaluHeader>;

/// H265 levels as defined by table A.8.
/// `general_level_idc` and `sub_layer_level_idc[ OpTid ]` shall be set equal to a
/// value of 30 times the level number specified in Table A.8
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
	#[default]
	L1 = 30,
	L2 = 60,
	L2_1 = 63,
	L3 = 90,
	L3_1 = 93,
	L4 = 120,
	L4_1 = 123,
	L5 = 150,
	L5_1 = 153,
	L5_2 = 156,
	L6 = 180,
	L6_1 = 183,
	L6_2 = 186,
}

impl TryFrom<u8> for Level {
	type Error = String;

	fn try_from(value: u8) -> Result<Self, Self::Error> {
		match value {
			30 => Ok(Level::L1),
			60 => Ok(Level::L2),
			63 => Ok(Level::L2_1),
			90 => Ok(Level::L3),
			93 => Ok(Level::L3_1),
			120 => Ok(Level::L4),
			123 => Ok(Level::L4_1),
			150 => Ok(Level::L5),
			153 => Ok(Level::L5_1),
			156 => Ok(Level::L5_2),
			180 => Ok(Level::L6),
			183 => Ok(Level::L6_1),
			186 => Ok(Level::L6_2),
			_ => Err(format!("Invalid Level {}", value)),
		}
	}
}

/// H265 profiles. See A.3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
	#[default]
	Main = 1,
	Main10 = 2,
	MainStill = 3,
	RangeExtensions = 4,
	HighThroughput = 5,
	MultiviewMain = 6,
	ScalableMain = 7,
	ThreeDMain = 8,
	ScreenContentCoding = 9,
	ScalableRangeExtensions = 10,
	HighThroughputScreenContentCoding = 11,
}

impl TryFrom<u8> for Profile {
	type Error = String;

	fn try_from(value: u8) -> Result<Self, Self::Error> {
		match value {
			1 => Ok(Profile::Main),
			2 => Ok(Profile::Main10),
			3 => Ok(Profile::MainStill),
			4 => Ok(Profile::RangeExtensions),
			5 => Ok(Profile::HighThroughput),
			6 => Ok(Profile::MultiviewMain),
			7 => Ok(Profile::ScalableMain),
			8 => Ok(Profile::ThreeDMain),
			9 => Ok(Profile::ScreenContentCoding),
			10 => Ok(Profile::ScalableRangeExtensions),
			11 => Ok(Profile::HighThroughputScreenContentCoding),
			_ => Err(format!("Invalid Profile {}", value)),
		}
	}
}

/// A H.265 Video Parameter Set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vps {
	/// Identifies the VPS for reference by other syntax elements.
	pub video_parameter_set_id: u8,
	/// If vps_base_layer_internal_flag is equal to 1 and
	/// vps_base_layer_available_flag is equal to 1, the base layer is present
	/// in the bitstream.
	pub base_layer_internal_flag: bool,
	/// See `base_layer_internal_flag`.
	pub base_layer_available_flag: bool,
	/// Plus 1 specifies the maximum allowed number of layers in each CVS
	/// referring to the VPS.
	pub max_layers_minus1: u8,
	/// Plus 1 specifies the maximum number of temporal sub-layers that may be
	/// present in each CVS referring to the VPS.
	pub max_sub_layers_minus1: u8,
	/// When vps_max_sub_layers_minus1 is greater than 0, specifies whether
	/// inter prediction is additionally restricted for CVSs referring to the
	/// VPS.
	pub temporal_id_nesting_flag: bool,
	/// ProfileTierLevel() data.
	pub profile_tier_level: ProfileTierLevel,
	/// When true, specifies that `vps_max_dec_pic_buffering_minus1[ i ]`,
	/// `vps_max_num_reorder_pics[ i ]` and `vps_max_latency_increase_plus1[ i ]`
	/// are present for vps_max_sub_layers_ minus1 + 1 sub-layers.
	/// vps_sub_layer_ordering_info_present_flag equal to 0 specifies that the
	/// values of `vps_max_dec_pic_buffering_minus1[ vps_max_sub_layers_minus1 ]`,
	/// vps_max_num_reorder_pics[ vps_max_sub_ layers_minus1 ] and
	/// `vps_max_latency_increase_plus1[ vps_max_sub_layers_minus1 ]` apply to all
	/// sub-layers
	pub sub_layer_ordering_info_present_flag: bool,
	/// `max_dec_pic_buffering_minus1[i]` plus 1 specifies the maximum required
	/// size of the decoded picture buffer for the CVS in units of picture
	/// storage buffers when HighestTid is equal to i.
	pub max_dec_pic_buffering_minus1: [u32; 7],
	/// Indicates the maximum allowed number of pictures with PicOutputFlag
	/// equal to 1 that can precede any picture with PicOutputFlag equal to 1 in
	/// the CVS in decoding order and follow that picture with PicOutputFlag
	/// equal to 1 in output order when HighestTid is equal to i.
	pub max_num_reorder_pics: [u32; 7],
	/// When true, `max_latency_increase_plus1[i]` is used to compute the value of
	/// `VpsMaxLatencyPictures[ i ]`, which specifies the maximum number of
	/// pictures with PicOutputFlag equal to 1 that can precede any picture with
	/// PicOutputFlag equal to 1 in the CVS in output order and follow that
	/// picture with PicOutputFlag equal to 1 in decoding order when HighestTid
	/// is equal to i.
	pub max_latency_increase_plus1: [u32; 7],
	/// Specifies the maximum allowed value of nuh_layer_id of all NAL units in
	/// each CVS referring to the VPS.
	pub max_layer_id: u8,
	/// num_layer_sets_minus1 plus 1 specifies the number of layer sets that are
	/// specified by the VPS.
	pub num_layer_sets_minus1: u32,
	/// When true, specifies that num_units_in_tick, time_scale,
	/// poc_proportional_to_timing_flag and num_hrd_parameters are present in
	/// the VPS.
	pub timing_info_present_flag: bool,
	/// The number of time units of a clock operating at the frequency
	/// vps_time_scale Hz that corresponds to one increment (called a clock
	/// tick) of a clock tick counter. The value of vps_num_units_in_tick shall
	/// be greater than 0. A clock tick, in units of seconds, is equal to the
	/// quotient of vps_num_units_in_tick divided by vps_time_scale. For
	/// example, when the picture rate of a video signal is 25 Hz,
	/// vps_time_scale may be equal to 27 000 000 and vps_num_units_in_tick may
	/// be equal to 1 080 000, and consequently a clock tick may be 0.04
	/// seconds.
	pub num_units_in_tick: u32,
	/// The number of time units that pass in one second. For example, a time
	/// coordinate system that measures time using a 27 MHz clock has a
	/// vps_time_scale of 27 000 000.
	pub time_scale: u32,
	/// When true, indicates that the picture order count value for each picture
	/// in the CVS that is not the first picture in the CVS, in decoding order,
	/// is proportional to the output time of the picture relative to the output
	/// time of the first picture in the CVS.  When false, indicates that the
	/// picture order count value for each picture in the CVS that is not the
	/// first picture in the CVS, in decoding order, may or may not be
	/// proportional to the output time of the picture relative to the output
	/// time of the first picture in the CVS.
	pub poc_proportional_to_timing_flag: bool,
	/// num_ticks_poc_diff_one_minus1 plus 1 specifies the number of clock ticks
	/// corresponding to a difference of picture order count values equal to 1.
	pub num_ticks_poc_diff_one_minus1: u32,
	/// Specifies the number of hrd_parameters( ) syntax structures present in
	/// the VPS RBSP before the vps_extension_flag syntax element.
	pub num_hrd_parameters: u32,
	/// `hrd_layer_set_idx[ i ]` specifies the index, into the list of layer sets
	/// specified by the VPS, of the layer set to which the i-th hrd_parameters(
	/// ) syntax structure in the VPS applies.
	pub hrd_layer_set_idx: Vec<u16>,
	/// `cprms_present_flag[ i ]` equal to true specifies that the HRD parameters
	/// that are common for all sub-layers are present in the i-th
	/// hrd_parameters( ) syntax structure in the VPS. `cprms_present_flag[ i ]`
	/// equal to false specifies that the HRD parameters that are common for all
	/// sub-layers are not present in the i-th hrd_parameters( ) syntax
	/// structure in the VPS and are derived to be the same as the ( i − 1 )-th
	/// hrd_parameters( ) syntax structure in the VPS. `cprms_present_flag[ 0 ]`
	/// is inferred to be equal to true.
	pub cprms_present_flag: Vec<bool>,
	/// The hrd_parameters() data.
	pub hrd_parameters: Vec<HrdParams>,
	/// When false, specifies that no vps_extension_data_flag syntax elements
	/// are present in the VPS RBSP syntax structure. When true, specifies that
	/// there are vps_extension_data_flag syntax elements present in the VPS
	/// RBSP syntax structure. Decoders conforming to a profile specified in
	/// Annex A but not supporting the INBLD capability specified in Annex F
	/// shall ignore all data that follow the value 1 for vps_extension_flag in
	/// a VPS NAL unit.
	pub extension_flag: bool,
}

impl Default for Vps {
	fn default() -> Self {
		Self {
			video_parameter_set_id: Default::default(),
			base_layer_internal_flag: Default::default(),
			base_layer_available_flag: Default::default(),
			max_layers_minus1: Default::default(),
			max_sub_layers_minus1: Default::default(),
			temporal_id_nesting_flag: Default::default(),
			profile_tier_level: Default::default(),
			sub_layer_ordering_info_present_flag: Default::default(),
			max_dec_pic_buffering_minus1: Default::default(),
			max_num_reorder_pics: Default::default(),
			max_latency_increase_plus1: Default::default(),
			max_layer_id: Default::default(),
			num_layer_sets_minus1: Default::default(),
			timing_info_present_flag: Default::default(),
			num_units_in_tick: Default::default(),
			time_scale: Default::default(),
			poc_proportional_to_timing_flag: Default::default(),
			num_ticks_poc_diff_one_minus1: Default::default(),
			num_hrd_parameters: Default::default(),
			hrd_layer_set_idx: Default::default(),
			cprms_present_flag: vec![true],
			hrd_parameters: Default::default(),
			extension_flag: Default::default(),
		}
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileTierLevel {
	/// Specifies the context for the interpretation of general_profile_idc and
	/// `general_profile_compatibility_flag[ j ]` for all values of j in the range
	/// of 0 to 31, inclusive.
	pub general_profile_space: u8,
	/// Specifies the tier context for the interpretation of general_level_idc
	/// as specified in Annex A.
	pub general_tier_flag: bool,
	/// When general_profile_space is equal to 0, indicates a profile to which
	/// the CVS conforms as specified in Annex A. Bitstreams shall not contain
	/// values of general_profile_idc other than those specified in Annex A.
	/// Other values of general_profile_idc are reserved for future use by ITU-T
	/// | ISO/IEC.
	pub general_profile_idc: u8,
	/// `general_profile_compatibility_flag[ j ]` equal to true, when
	/// general_profile_space is false, indicates that the CVS conforms to the
	/// profile indicated by general_profile_idc equal to j as specified in
	/// Annex A.
	pub general_profile_compatibility_flag: [bool; 32],
	/// general_progressive_source_flag and general_interlaced_source_flag are
	/// interpreted as follows:
	///
	/// –If general_progressive_source_flag is true and
	/// general_interlaced_source_flag is false, the source scan type of the
	/// pictures in the CVS should be interpreted as progressive only.
	///
	/// –Otherwise, if general_progressive_source_flag is false and
	/// general_interlaced_source_flag is true, the source scan type of the
	/// pictures in the CVS should be interpreted as interlaced only.
	///
	/// –Otherwise, if general_progressive_source_flag is false and
	/// general_interlaced_source_flag is false, the source scan type of the
	/// pictures in the CVS should be interpreted as unknown or unspecified.
	///
	/// –Otherwise (general_progressive_source_flag is true and
	/// general_interlaced_source_flag is true), the source scan type of each
	/// picture in the CVS is indicated at the picture level using the syntax
	/// element source_scan_type in a picture timing SEI message.
	pub general_progressive_source_flag: bool,
	/// See `general_progressive_source_flag`.
	pub general_interlaced_source_flag: bool,
	/// If true, specifies that there are no frame packing arrangement SEI
	/// messages, segmented rectangular frame packing arrangement SEI messages,
	/// equirectangular projection SEI messages, or cubemap projection SEI
	/// messages present in the CVS. If false, indicates that there may or may
	/// not be one or more frame packing arrangement SEI messages, segmented
	/// rectangular frame packing arrangement SEI messages, equirectangular
	/// projection SEI messages, or cubemap projection SEI messages present in
	/// the CVS.
	pub general_non_packed_constraint_flag: bool,
	/// When true, specifies that field_seq_flag is false. When false, indicates
	/// that field_seq_flag may or may not be false.
	pub general_frame_only_constraint_flag: bool,
	/// See Annex A.
	pub general_max_12bit_constraint_flag: bool,
	/// See Annex A.
	pub general_max_10bit_constraint_flag: bool,
	/// See Annex A.
	pub general_max_8bit_constraint_flag: bool,
	/// See Annex A.
	pub general_max_422chroma_constraint_flag: bool,
	/// See Annex A.
	pub general_max_420chroma_constraint_flag: bool,
	/// See Annex A.
	pub general_max_monochrome_constraint_flag: bool,
	/// See Annex A.
	pub general_intra_constraint_flag: bool,
	/// See Annex A.
	pub general_lower_bit_rate_constraint_flag: bool,
	/// See Annex A.
	pub general_max_14bit_constraint_flag: bool,
	/// See Annex A.
	pub general_one_picture_only_constraint_flag: bool,
	/// When true, specifies that the INBLD capability as specified in Annex F
	/// is required for decoding of the layer to which the profile_tier_level( )
	/// syntax structure applies. When false, specifies that the INBLD
	/// capability as specified in Annex F is not required for decoding of the
	/// layer to which the profile_tier_level( ) syntax structure applies.
	pub general_inbld_flag: bool,
	/// Indicates a level to which the CVS conforms as specified in Annex A.
	pub general_level_idc: Level,
	/// Sub-layer syntax element.
	pub sub_layer_profile_present_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_level_present_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_profile_space: [u8; 6],
	/// Sub-layer syntax element.
	pub sub_layer_tier_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_profile_idc: [u8; 6],
	/// Sub-layer syntax element.
	pub sub_layer_profile_compatibility_flag: [[bool; 32]; 6],
	/// Sub-layer syntax element.
	pub sub_layer_progressive_source_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_interlaced_source_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_non_packed_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_frame_only_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_12bit_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_10bit_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_8bit_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_422chroma_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_420chroma_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_monochrome_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_intra_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_one_picture_only_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_lower_bit_rate_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_max_14bit_constraint_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_inbld_flag: [bool; 6],
	/// Sub-layer syntax element.
	pub sub_layer_level_idc: [Level; 6],
}

impl ProfileTierLevel {
	pub fn max_luma_ps(&self) -> u32 {
		// See Table A.8.
		match self.general_level_idc {
			Level::L1 => 36864,
			Level::L2 => 122880,
			Level::L2_1 => 245760,
			Level::L3 => 552960,
			Level::L3_1 => 983040,
			Level::L4 | Level::L4_1 => 2228224,
			Level::L5 | Level::L5_1 | Level::L5_2 => 8912896,
			_ => 35651584,
		}
	}

	pub fn max_dpb_pic_buf(&self) -> u32 {
		if self.general_profile_idc >= 1 && self.general_profile_idc <= 5 {
			6
		} else {
			7
		}
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpsRangeExtension {
	pub transform_skip_rotation_enabled_flag: bool,
	pub transform_skip_context_enabled_flag: bool,
	pub implicit_rdpcm_enabled_flag: bool,
	pub explicit_rdpcm_enabled_flag: bool,
	pub extended_precision_processing_flag: bool,
	pub intra_smoothing_disabled_flag: bool,
	pub high_precision_offsets_enabled_flag: bool,
	pub persistent_rice_adaptation_enabled_flag: bool,
	pub cabac_bypass_alignment_enabled_flag: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpsSccExtension {
	/// When set, specifies that a picture in the CVS may be included in a
	/// reference picture list of a slice of the picture itself.  When not set,
	/// specifies that a picture in the CVS is never included in a reference
	/// picture list of a slice of the picture itself.
	pub curr_pic_ref_enabled_flag: bool,
	/// When set, specifies that the decoding process for palette mode may be
	/// used for intra blocks. When not set, specifies that the decoding process
	/// for palette mode is not applied.
	pub palette_mode_enabled_flag: bool,
	/// Specifies the maximum allowed palette size.
	pub palette_max_size: u8,
	/// Specifies the difference between the maximum allowed palette predictor
	/// size and the maximum allowed palette size.
	pub delta_palette_max_predictor_size: u8,
	/// When set, specifies that the sequence palette predictors are initialized
	/// using the sps_palette_predictor_initializers. When not set, specifies
	/// that the entries in the sequence palette predictor are initialized to 0.
	pub palette_predictor_initializers_present_flag: bool,
	/// num_palette_predictor_initializers_minus1 plus 1 specifies the number of
	/// entries in the sequence palette predictor initializer.
	pub num_palette_predictor_initializer_minus1: u8,
	/// `palette_predictor_initializer[ comp ][ i ]` specifies the value of the
	/// comp-th component of the i-th palette entry in the SPS that is used to
	/// initialize the array PredictorPaletteEntries.
	pub palette_predictor_initializer: [[u32; 128]; 3],
	/// Controls the presence and inference of the use_integer_mv_flag that
	/// specifies the resolution of motion vectors for inter prediction.
	pub motion_vector_resolution_control_idc: u8,
	/// When set, specifies that the intra boundary filtering process is
	/// unconditionally disabled for intra prediction.  If not set, specifies
	/// that the intra boundary filtering process may be used.
	pub intra_boundary_filtering_disabled_flag: bool,
}

impl Default for SpsSccExtension {
	fn default() -> Self {
		Self {
			curr_pic_ref_enabled_flag: Default::default(),
			palette_mode_enabled_flag: Default::default(),
			palette_max_size: Default::default(),
			delta_palette_max_predictor_size: Default::default(),
			palette_predictor_initializers_present_flag: Default::default(),
			num_palette_predictor_initializer_minus1: Default::default(),
			palette_predictor_initializer: [[0; 128]; 3],
			motion_vector_resolution_control_idc: Default::default(),
			intra_boundary_filtering_disabled_flag: Default::default(),
		}
	}
}

/// A H.265 Sequence Parameter Set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sps {
	/// Specifies the value of the vps_video_parameter_set_id of the active VPS.
	pub video_parameter_set_id: u8,
	/// `max_sub_layers_minus1` plus 1 specifies the maximum number of temporal
	/// sub-layers that may be present in each CVS referring to the SPS.
	pub max_sub_layers_minus1: u8,
	/// When sps_max_sub_layers_minus1 is greater than 0, specifies whether
	/// inter prediction is additionally restricted for CVSs referring to the
	/// SPS.
	pub temporal_id_nesting_flag: bool,
	/// profile_tier_level() data.
	pub profile_tier_level: ProfileTierLevel,
	/// Provides an identifier for the SPS for reference by other syntax
	/// elements.
	pub seq_parameter_set_id: u8,
	/// Specifies the chroma sampling relative to the luma sampling as specified
	/// in clause 6.2.
	pub chroma_format_idc: u8,
	/// When true, specifies that the three colour components of the 4:4:4
	/// chroma format are coded separately. When false, specifies that the
	/// colour components are not coded separately.
	pub separate_colour_plane_flag: bool,
	/// Specifies the width of each decoded picture in units of luma samples.
	pub pic_width_in_luma_samples: u16,
	/// Specifies the height of each decoded picture in units of luma samples.
	pub pic_height_in_luma_samples: u16,
	/// When true, indicates that the conformance cropping window offset
	/// parameters follow next in the SPS. When false, indicates that the
	/// conformance cropping window offset parameters are not present.
	pub conformance_window_flag: bool,
	/* if conformance_window_flag */
	/// Specify the samples of the pictures in the CVS that are output from the
	/// decoding process, in terms of a rectangular region specified in picture
	/// coordinates for output.
	pub conf_win_left_offset: u32,
	pub conf_win_right_offset: u32,
	pub conf_win_top_offset: u32,
	pub conf_win_bottom_offset: u32,

	/// Specifies the bit depth of the samples of the luma array BitDepthY and
	/// the value of the luma quantization parameter range offset QpBdOffsetY.
	pub bit_depth_luma_minus8: u8,
	/// Specifies the bit depth of the samples of the chroma arrays BitDepthC
	/// and the value of the chroma quantization parameter range offset
	/// QpBdOffsetC.
	pub bit_depth_chroma_minus8: u8,
	/// Specifies the value of the variable MaxPicOrderCntLsb that is used in
	/// the decoding process for picture order count.
	pub log2_max_pic_order_cnt_lsb_minus4: u8,
	/// When true, specifies that `max_dec_pic_buffering_minus1[ i ]`,
	/// `max_num_reorder_pics[ i ]` and `max_latency_increase_plus1[ i ]` are
	/// present for max_sub_layers_minus1 + 1 sub- layers. When false, specifies
	/// that the values of `max_dec_pic_ buffering_minus1[ max_sub_layers_minus1
	/// ]`, `max_num_reorder_pics[ max_sub_layers_minus1 ]` and max_
	/// `latency_increase_plus1[ max_sub_layers_minus1 ]` apply to all sub-layers.
	pub sub_layer_ordering_info_present_flag: bool,
	/// `max_dec_pic_buffering_minus1[ i ]` plus 1 specifies the maximum required
	/// size of the decoded picture buffer for the CVS in units of picture
	/// storage buffers when HighestTid is equal to i.
	pub max_dec_pic_buffering_minus1: [u8; 7],
	/// `max_num_reorder_pics[ i ]` indicates the maximum allowed number of
	/// pictures with PicOutputFlag equal to 1 that can precede any picture with
	/// PicOutputFlag equal to 1 in the CVS in decoding order and follow that
	/// picture with PicOutputFlag equal to 1 in output order when HighestTid is
	/// equal to i.
	pub max_num_reorder_pics: [u8; 7],
	/// `max_latency_increase_plus1[ i ]` not equal to 0 is used to compute the
	/// value of `SpsMaxLatencyPictures[ i ]`, which specifies the maximum number
	/// of pictures with PicOutputFlag equal to 1 that can precede any picture
	/// with PicOutputFlag equal to 1 in the CVS in output order and follow that
	/// picture with PicOutputFlag equal to 1 in decoding order when HighestTid
	/// is equal to i.
	pub max_latency_increase_plus1: [u8; 7],
	/// min_luma_coding_block_size_minus3 plus 3 specifies the minimum luma
	/// coding block size.
	pub log2_min_luma_coding_block_size_minus3: u8,
	/// Specifies the difference between the maximum and minimum luma coding
	/// block size.
	pub log2_diff_max_min_luma_coding_block_size: u8,
	/// min_luma_transform_block_size_minus2 plus 2 specifies the minimum luma
	/// transform block size.
	pub log2_min_luma_transform_block_size_minus2: u8,
	/// Specifies the difference between the maximum and minimum luma transform
	/// block size.
	pub log2_diff_max_min_luma_transform_block_size: u8,
	/// Specifies the maximum hierarchy depth for transform units of coding
	/// units coded in inter prediction mode.
	pub max_transform_hierarchy_depth_inter: u8,
	/// Specifies the maximum hierarchy depth for transform units of coding
	/// units coded in intra prediction mode.
	pub max_transform_hierarchy_depth_intra: u8,
	/// When true, specifies that a scaling list is used for the scaling process
	/// for transform coefficients. When false, specifies that scaling list is
	/// not used for the scaling process for transform coefficients.
	pub scaling_list_enabled_flag: bool,
	/* if scaling_list_enabled_flag */
	/// When true, specifies that the scaling_list_data( ) syntax structure is
	/// present in the SPS. When false, specifies that the scaling_list_data( )
	/// syntax structure is not present in the SPS.
	pub scaling_list_data_present_flag: bool,
	/// The scaling_list_data() syntax data.
	pub scaling_list: ScalingLists,
	/// When true, specifies that asymmetric motion partitions, i.e., PartMode
	/// equal to PART_2NxnU, PART_2NxnD, PART_nLx2N or PART_nRx2N, may be used
	/// in CTBs. When false, specifies that asymmetric motion partitions cannot
	/// be used in CTBs.
	pub amp_enabled_flag: bool,
	/// When true, specifies that the sample adaptive offset process is applied
	/// to the reconstructed picture after the deblocking filter process.  When
	/// false, specifies that the sample adaptive offset process is not applied
	/// to the reconstructed picture after the deblocking filter process.
	pub sample_adaptive_offset_enabled_flag: bool,
	/// When false, specifies that PCM-related syntax
	/// (pcm_sample_bit_depth_luma_minus1, pcm_sample_ bit_depth_chroma_minus1,
	/// log2_min_pcm_luma_coding_block_size_minus3, log2_diff_max_min_pcm_luma_
	/// coding_block_size, pcm_loop_filter_disabled_flag, pcm_flag,
	/// pcm_alignment_zero_bit syntax elements and pcm_sample( ) syntax
	/// structure) is not present in the CVS.
	pub pcm_enabled_flag: bool,

	/* if pcm_enabled_flag */
	pub pcm_sample_bit_depth_luma_minus1: u8,
	/// Specifies the number of bits used to represent each of PCM sample values
	/// of the luma component.
	pub pcm_sample_bit_depth_chroma_minus1: u8,
	/// Specifies the number of bits used to represent each of PCM sample values
	/// of the chroma components.
	pub log2_min_pcm_luma_coding_block_size_minus3: u8,
	/// Specifies the difference between the maximum and minimum size of coding
	/// blocks with pcm_flag equal to true.
	pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
	/// Specifies whether the loop filter process is disabled on reconstructed
	/// samples in a coding unit with pcm_flag equal to true as follows:
	///
	/// – If pcm_loop_filter_disabled_flag is set, the deblocking filter and
	/// sample adaptive offset filter processes on the reconstructed samples in
	/// a coding unit with pcm_flag set are disabled.
	///
	/// – Otherwise (pcm_loop_filter_disabled_flag value is not set), the
	/// deblocking filter and sample adaptive offset filter processes on the
	/// reconstructed samples in a coding unit with pcm_flag set are not
	/// disabled.
	pub pcm_loop_filter_disabled_flag: bool,
	/// Specifies the number of st_ref_pic_set( ) syntax structures included in
	/// the SPS.
	pub num_short_term_ref_pic_sets: u8,
	/// the st_ref_pic_set() data.
	pub short_term_ref_pic_set: Vec<ShortTermRefPicSet>,
	/// If unset, specifies that no long-term reference picture is used for
	/// inter prediction of any coded picture in the CVS.
	/// If set, specifies that long-term reference pictures may be used for
	/// inter prediction of one or more coded pictures in the CVS.
	pub long_term_ref_pics_present_flag: bool,

	/* if long_term_ref_pics_present_flag */
	/// Specifies the number of candidate long-term reference pictures that are
	/// specified in the SPS.
	pub num_long_term_ref_pics_sps: u8,
	/// `lt_ref_pic_poc_lsb_sps[ i ]` specifies the picture order count modulo
	/// MaxPicOrderCntLsb of the i-th candidate long-term reference picture
	/// specified in the SPS.
	pub lt_ref_pic_poc_lsb_sps: [u32; MAX_LONG_TERM_REF_PIC_SETS],
	/// `used_by_curr_pic_lt_sps_flag[ i ]` equal to false specifies that the i-th
	/// candidate long-term reference picture specified in the SPS is not used
	/// for reference by a picture that includes in its long-term reference
	/// picture set (RPS) the i-th candidate long-term reference picture
	/// specified in the SPS.
	pub used_by_curr_pic_lt_sps_flag: [bool; MAX_LONG_TERM_REF_PIC_SETS],
	/// When set, specifies that slice_temporal_mvp_enabled_flag is present in
	/// the slice headers of non-IDR pictures in the CVS. When not set,
	/// specifies that slice_temporal_mvp_enabled_flag is not present in slice
	/// headers and that temporal motion vector predictors are not used in the
	/// CVS.
	pub temporal_mvp_enabled_flag: bool,
	/// When set, specifies that bi-linear interpolation is conditionally used
	/// in the intraprediction filtering process in the CVS as specified in
	/// clause 8.4.4.2.3.
	pub strong_intra_smoothing_enabled_flag: bool,
	/// When set, specifies that the vui_parameters( ) syntax structure as
	/// specified in Annex E is present. When not set, specifies that the
	/// vui_parameters( ) syntax structure as specified in Annex E is not
	/// present.
	pub vui_parameters_present_flag: bool,
	/// The vui_parameters() data.
	pub vui_parameters: VuiParams,
	/// When set, specifies that the syntax elements sps_range_extension_flag,
	/// sps_multilayer_extension_flag, sps_3d_extension_flag,
	/// sps_scc_extension_flag, and sps_extension_4bits are present in the SPS
	/// RBSP syntax structure. When not set, specifies that these syntax
	/// elements are not present.
	pub extension_present_flag: bool,

	pub range_extension_flag: bool,
	/// The sps_range_extension() data.
	pub range_extension: SpsRangeExtension,
	/// When set, specifies that the sps_scc_extension( ) syntax structure is
	/// present in the SPS RBSP syntax structure. When not set, specifies that
	/// this syntax structure is not present
	pub scc_extension_flag: bool,
	/// The sps_scc_extension() data.
	pub scc_extension: SpsSccExtension,

	// Internal H265 variables. Computed from the bitstream.
	/// Equivalent to MinCbLog2SizeY in the specification.
	pub min_cb_log2_size_y: u32,
	/// Equivalent to CtbLog2SizeY in the specification.
	pub ctb_log2_size_y: u32,
	/// Equivalent to CtbSizeY in the specification.
	pub ctb_size_y: u32,
	/// Equivalent to PicHeightInCtbsY in the specification.
	pub pic_height_in_ctbs_y: u32,
	/// Equivalent to PicWidthInCtbsY in the specification.
	pub pic_width_in_ctbs_y: u32,
	/// Equivalent to PicSizeInCtbsY in the specification.
	pub pic_size_in_ctbs_y: u32,
	/// Equivalent to ChromaArrayType in the specification.
	pub chroma_array_type: u8,
	/// Equivalent to WpOffsetHalfRangeY in the specification.
	pub wp_offset_half_range_y: u32,
	/// Equivalent to WpOffsetHalfRangeC in the specification.
	pub wp_offset_half_range_c: u32,
	/// Equivalent to MaxTbLog2SizeY in the specification.
	pub max_tb_log2_size_y: u32,
	/// Equivalent to PicSizeInSamplesY in the specification.
	pub pic_size_in_samples_y: u32,

	/// The VPS referenced by this SPS, if any.
	pub vps: Option<Rc<Vps>>,
}

impl Sps {
	pub fn max_dpb_size(&self) -> usize {
		let max_luma_ps = self.profile_tier_level.max_luma_ps();
		let max_dpb_pic_buf = self.profile_tier_level.max_dpb_pic_buf();

		// Equation A-2
		let max = if self.pic_size_in_samples_y <= (max_luma_ps >> 2) {
			std::cmp::min(4 * max_dpb_pic_buf, 16)
		} else if self.pic_size_in_samples_y <= (max_luma_ps >> 1) {
			std::cmp::min(2 * max_dpb_pic_buf, 16)
		} else if self.pic_size_in_samples_y <= ((3 * max_luma_ps) >> 2) {
			std::cmp::min(4 * max_dpb_pic_buf / 3, 16)
		} else {
			max_dpb_pic_buf
		};

		max as usize
	}

	pub fn width(&self) -> u16 {
		self.pic_width_in_luma_samples
	}

	pub fn height(&self) -> u16 {
		self.pic_height_in_luma_samples
	}

	pub fn visible_rectangle(&self) -> Rect<u32> {
		// From the specification:
		// NOTE 3 – The conformance cropping window offset parameters are
		// only applied at the output. All internal decoding processes are
		// applied to the uncropped picture size.
		if !self.conformance_window_flag {
			return Rect {
				min: Point { x: 0, y: 0 },
				max: Point {
					x: u32::from(self.width()),
					y: u32::from(self.height()),
				},
			};
		}
		const SUB_HEIGHT_C: [u32; 5] = [1, 2, 1, 1, 1];
		const SUB_WIDTH_C: [u32; 5] = [1, 2, 2, 1, 1];

		let crop_unit_y = SUB_HEIGHT_C[usize::from(self.chroma_array_type)];
		let crop_unit_x = SUB_WIDTH_C[usize::from(self.chroma_array_type)];
		let crop_left = crop_unit_x * self.conf_win_left_offset;
		let crop_right = crop_unit_x * self.conf_win_right_offset;
		let crop_top = crop_unit_y * self.conf_win_top_offset;
		let crop_bottom = crop_unit_y * self.conf_win_bottom_offset;

		Rect {
			min: Point {
				x: crop_left,
				y: crop_top,
			},
			max: Point {
				x: u32::from(self.width()) - crop_left - crop_right,
				y: u32::from(self.height()) - crop_top - crop_bottom,
			},
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PpsSccExtension {
	/// When set, specifies that a picture referring to the PPS may be included
	/// in a reference picture list of a slice of the picture itself.  If not
	/// set, specifies that a picture referring to the PPS is never included in
	/// a reference picture list of a slice of the picture itself.
	pub curr_pic_ref_enabled_flag: bool,
	/// When set, specifies that an adaptive colour transform may be applied to
	/// the residual in the decoding process. When not set, specifies that
	/// adaptive colour transform is not applied to the residual.
	pub residual_adaptive_colour_transform_enabled_flag: bool,
	/// When set, specifies that slice_act_y_qp_offset, slice_act_cb_qp_offset,
	/// slice_act_cr_qp_offset are present in the slice header.  When not set,
	/// specifies that slice_act_y_qp_offset, slice_act_cb_qp_offset,
	/// slice_act_cr_qp_offset are not present in the slice header.
	pub slice_act_qp_offsets_present_flag: bool,
	/// See the specificartion for more details.
	pub act_y_qp_offset_plus5: i8,
	/// See the specificartion for more details.
	pub act_cb_qp_offset_plus5: i8,
	/// See the specificartion for more details.
	pub act_cr_qp_offset_plus3: i8,
	/// When set, specifies that the palette predictor initializers used for the
	/// pictures referring to the PPS are derived based on the palette predictor
	/// initializers specified by the PPS. If not set, specifies that the
	/// palette predictor initializers used for the pictures referring to the
	/// PPS are inferred to be equal to those specified by the active SPS.
	pub palette_predictor_initializers_present_flag: bool,
	/// Specifies the number of entries in the picture palette predictor
	/// initializer.
	pub num_palette_predictor_initializers: u8,
	/// When set, specifies that the pictures that refer to this PPS are
	/// monochrome. If not set, specifies that the pictures that refer to this
	/// PPS have multiple components.
	pub monochrome_palette_flag: bool,
	/// luma_bit_depth_entry_minus8 plus 8 specifies the bit depth of the luma
	/// component of the entries of the palette predictor initializer.
	pub luma_bit_depth_entry_minus8: u8,
	/// chroma_bit_depth_entry_minus8 plus 8 specifies the bit depth of the
	/// chroma components of the entries of the palette predictor initializer.
	pub chroma_bit_depth_entry_minus8: u8,
	/// `pps_palette_predictor_initializer[ comp ][ i ]` specifies the value of
	/// the comp-th component of the i-th palette entry in the PPS that is used
	/// to initialize the array PredictorPaletteEntries.
	pub palette_predictor_initializer: [[u8; 128]; 3],
}

impl Default for PpsSccExtension {
	fn default() -> Self {
		Self {
			curr_pic_ref_enabled_flag: Default::default(),
			residual_adaptive_colour_transform_enabled_flag: Default::default(),
			slice_act_qp_offsets_present_flag: Default::default(),
			act_y_qp_offset_plus5: Default::default(),
			act_cb_qp_offset_plus5: Default::default(),
			act_cr_qp_offset_plus3: Default::default(),
			palette_predictor_initializers_present_flag: Default::default(),
			num_palette_predictor_initializers: Default::default(),
			monochrome_palette_flag: Default::default(),
			luma_bit_depth_entry_minus8: Default::default(),
			chroma_bit_depth_entry_minus8: Default::default(),
			palette_predictor_initializer: [[0; 128]; 3],
		}
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PpsRangeExtension {
	/// log2_max_transform_skip_block_size_minus2 plus 2 specifies the maximum
	/// transform block size for which transform_skip_flag may be present in
	/// coded pictures referring to the PPS. When not present, the value of
	/// log2_max_transform_skip_block_size_minus2 is inferred to be equal to 0.
	/// When present, the value of log2_max_transform_skip_block_size_minus2
	/// shall be less than or equal to MaxTbLog2SizeY − 2.
	pub log2_max_transform_skip_block_size_minus2: u32,
	/// When set, specifies that log2_res_scale_abs_plus1 and
	/// res_scale_sign_flag may be present in the transform unit syntax for
	/// pictures referring to the PPS. When not set, specifies that
	/// log2_res_scale_abs_plus1 and res_scale_sign_flag are not present for
	/// pictures referring to the PPS.
	pub cross_component_prediction_enabled_flag: bool,
	/// When set, specifies that the cu_chroma_qp_offset_flag may be present in
	/// the transform unit syntax. When not set, specifies that the
	/// cu_chroma_qp_offset_flag is not present in the transform unit syntax.
	pub chroma_qp_offset_list_enabled_flag: bool,
	/// Specifies the difference between the luma CTB size and the minimum luma
	/// coding block size of coding units that convey cu_chroma_qp_offset_flag.
	pub diff_cu_chroma_qp_offset_depth: u32,
	/// chroma_qp_offset_list_len_minus1 plus 1 specifies the number of
	/// `cb_qp_offset_list[ i ]` and `cr_qp_offset_list[ i ]` syntax elements that
	/// are present in the PPS.
	pub chroma_qp_offset_list_len_minus1: u32,
	/// Specify offsets used in the derivation of Qp′Cb and Qp′Cr, respectively.
	pub cb_qp_offset_list: [i32; 6],
	/// Specify offsets used in the derivation of Qp′Cb and Qp′Cr, respectively.
	pub cr_qp_offset_list: [i32; 6],
	/// The base 2 logarithm of the scaling parameter that is used to scale
	/// sample adaptive offset (SAO) offset values for luma samples.
	pub log2_sao_offset_scale_luma: u32,
	/// The base 2 logarithm of the scaling parameter that is used to scale SAO
	/// offset values for chroma samples.
	pub log2_sao_offset_scale_chroma: u32,
}

/// A H.265 Picture Parameter Set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pps {
	/// Identifies the PPS for reference by other syntax elements.
	pub pic_parameter_set_id: u8,
	/// Specifies the value of sps_seq_parameter_set_id for the active SPS.
	pub seq_parameter_set_id: u8,
	/// When set, specifies the presence of the syntax element
	/// dependent_slice_segment_flag in the slice segment headers for coded
	/// pictures referring to the PPS. When not set, specifies the absence of
	/// the syntax element dependent_slice_segment_flag in the slice segment
	/// headers for coded pictures referring to the PPS.
	pub dependent_slice_segments_enabled_flag: bool,
	/// When set, indicates that the pic_output_flag syntax element is present
	/// in the associated slice headers. When not set, indicates that the
	/// pic_output_flag syntax element is not present in the associated slice
	/// headers.
	pub output_flag_present_flag: bool,
	/// Specifies the number of extra slice header bits that are present in the
	/// slice header RBSP for coded pictures referring to the PPS.
	pub num_extra_slice_header_bits: u8,
	/// When not set, specifies that sign bit hiding is disabled. Whens set,
	/// specifies that sign bit hiding is enabled.
	pub sign_data_hiding_enabled_flag: bool,
	/// When set, specifies that cabac_init_flag is present in slice headers
	/// referring to the PPS. When not set, specifies that cabac_init_flag is
	/// not present in slice headers referring to the PPS.
	pub cabac_init_present_flag: bool,
	/// Specifies the inferred value of num_ref_idx_l0_active_minus1 for P and B
	/// slices with num_ref_idx_active_override_flag not set.
	pub num_ref_idx_l0_default_active_minus1: u8,
	/// Specifies the inferred value of num_ref_idx_l1_active_minus1 for B
	/// slices with num_ref_idx_active_override_flag not set.
	pub num_ref_idx_l1_default_active_minus1: u8,
	/// init_qp_minus26 plus 26 specifies the initial value of SliceQpY for each
	/// slice referring to the PPS. The initial value of SliceQpY is modified at
	/// the slice segment layer when a non-zero value of slice_qp_delta is
	/// decoded.
	pub init_qp_minus26: i8,
	/// When not set, specifies that intra prediction allows usage of residual
	/// data and decoded samples of neighbouring coding blocks coded using
	/// either intra or inter prediction modes. When set, specifies constrained
	/// intra prediction, in which case intra prediction only uses residual data
	/// and decoded samples from neighbouring coding blocks coded using intra
	/// prediction modes.
	pub constrained_intra_pred_flag: bool,
	/// When set, specifies that transform_skip_flag may be present in the
	/// residual coding syntax. When not set, specifies that transform_skip_flag
	/// is not present in the residual coding syntax.
	pub transform_skip_enabled_flag: bool,
	/// When set, specifies that the diff_cu_qp_delta_depth syntax element is
	/// present in the PPS and that cu_qp_delta_abs may be present in the
	/// transform unit syntax and the palette syntax. When not set, specifies
	/// that the diff_cu_qp_delta_depth syntax element is not present in the PPS
	/// and that cu_qp_delta_abs is not present in the transform unit syntax and
	/// the palette syntax.
	pub cu_qp_delta_enabled_flag: bool,

	/*if cu_qp_delta_enabled_flag */
	/// Specifies the difference between the luma CTB size and the minimum luma
	/// coding block size of coding units that convey cu_qp_delta_abs and
	/// cu_qp_delta_sign_flag.
	pub diff_cu_qp_delta_depth: u8,
	/// Specifies the offsets to the luma quantization parameter Qp′Y used for
	/// deriving Qp′Cb and Qp′Cr, respectively.
	pub cb_qp_offset: i8,
	/// Specifies the offsets to the luma quantization parameter Qp′Y used for
	/// deriving Qp′Cb and Qp′Cr, respectively.
	pub cr_qp_offset: i8,
	/// When set, indicates that the slice_cb_qp_offset and slice_cr_qp_offset
	/// syntax elements are present in the associated slice headers.  When not
	/// set, indicates that these syntax elements are not present in the
	/// associated slice headers. When ChromaArrayType is equal to 0,
	/// pps_slice_chroma_qp_offsets_present_flag shall be equal to 0
	pub slice_chroma_qp_offsets_present_flag: bool,
	/// When not set, specifies that weighted prediction is not applied to P
	/// slices. When set, specifies that weighted prediction is applied to P
	/// slices.
	pub weighted_pred_flag: bool,
	/// When not set, specifies that the default weighted prediction is applied
	/// to B slices. When set, specifies that weighted prediction is applied to
	/// B slices.
	pub weighted_bipred_flag: bool,
	/// When set, specifies that `cu_transquant_bypass_flag` is present, When
	/// not set, specifies that `cu_transquant_bypass_flag` is not present.
	pub transquant_bypass_enabled_flag: bool,
	/// When set, specifies that there is more than one tile in each picture
	/// referring to the PPS. When not set, specifies that there is only one
	/// tile in each picture referring to the PPS.
	pub tiles_enabled_flag: bool,
	/// When set, specifies that a specific synchronization process for context
	/// variables, and when applicable, Rice parameter initialization states and
	/// palette predictor variables, is invoked before decoding the CTU which
	/// includes the first CTB of a row of CTBs in each tile in each picture
	/// referring to the PPS, and a specific storage process for context
	/// variables, and when applicable, Rice parameter initialization states and
	/// palette predictor variables, is invoked after decoding the CTU which
	/// includes the second CTB of a row of CTBs in each tile in each picture
	/// referring to the PPS. When not set, specifies that no specific
	/// synchronization process for context variables, and when applicable, Rice
	/// parameter initialization states and palette predictor variables, is
	/// required to be invoked before decoding the CTU which includes the first
	/// CTB of a row of CTBs in each tile in each picture referring to the PPS,
	/// and no specific storage process for context variables, and when
	/// applicable, Rice parameter initialization states and palette predictor
	/// variables, is required to be invoked after decoding the CTU which
	/// includes the second CTB of a row of CTBs in each tile in each picture
	/// referring to the PPS.
	pub entropy_coding_sync_enabled_flag: bool,
	/// num_tile_columns_minus1 plus 1 specifies the number of tile columns
	/// partitioning the picture.
	pub num_tile_columns_minus1: u8,
	/// num_tile_rows_minus1 plus 1 specifies the number of tile rows
	/// partitioning the picture.
	pub num_tile_rows_minus1: u8,
	/// When set, specifies that tile column boundaries and likewise tile row
	/// boundaries are distributed uniformly across the picture.  When not set,
	/// specifies that tile column boundaries and likewise tile row boundaries
	/// are not distributed uniformly across the picture but signalled
	/// explicitly using the syntax elements `column_width_minus1[ i ]` and
	/// `row_height_minus1[ i ]`.
	pub uniform_spacing_flag: bool,
	/// `column_width_minus1[ i ]` plus 1 specifies the width of the i-th tile
	/// column in units of CTBs.
	pub column_width_minus1: [u32; MAX_TILE_COLUMNS],
	/// `row_height_minus1[ i ]` plus 1 specifies the height of the i-th tile row
	/// in units of CTBs.
	pub row_height_minus1: [u32; MAX_TILE_ROWS],
	/// When set, specifies that in-loop filtering operations may be performed
	/// across tile boundaries in pictures referring to the PPS.  When not set,
	/// specifies that in-loop filtering operations are not performed across
	/// tile boundaries in pictures referring to the PPS. The in-loop filtering
	/// operations include the deblocking filter and sample adaptive offset
	/// filter operations.
	pub loop_filter_across_tiles_enabled_flag: bool,
	/// When set, specifies that in-loop filtering operations may be performed
	/// across left and upper boundaries of slices referring to the PPS.  When
	/// not set, specifies that in-loop filtering operations are not performed
	/// across left and upper boundaries of slices referring to the PPS. The in-
	/// loop filtering operations include the deblocking filter and sample
	/// adaptive offset filter operations.
	pub loop_filter_across_slices_enabled_flag: bool,
	/// When set, specifies the presence of deblocking filter control syntax
	/// elements in the PPS. When not set, specifies the absence of deblocking
	/// filter control syntax elements in the PPS.
	pub deblocking_filter_control_present_flag: bool,
	/// When set, specifies the presence of deblocking_filter_override_flag in
	/// the slice headers for pictures referring to the PPS.  When not set,
	/// specifies the absence of deblocking_filter_override_flag in the slice
	/// headers for pictures referring to the PPS.
	pub deblocking_filter_override_enabled_flag: bool,
	/// When set, specifies that the operation of deblocking filter is not
	/// applied for slices referring to the PPS in which
	/// slice_deblocking_filter_disabled_flag is not present.  When not set,
	/// specifies that the operation of the deblocking filter is applied for
	/// slices referring to the PPS in which
	/// slice_deblocking_filter_disabled_flag is not present.
	pub deblocking_filter_disabled_flag: bool,
	/// Specify the default deblocking parameter offsets for β and tC (divided
	/// by 2) that are applied for slices referring to the PPS, unless the
	/// default deblocking parameter offsets are overridden by the deblocking
	/// parameter offsets present in the slice headers of the slices referring
	/// to the PPS.
	pub beta_offset_div2: i8,
	/// Specify the default deblocking parameter offsets for β and tC (divided
	/// by 2) that are applied for slices referring to the PPS, unless the
	/// default deblocking parameter offsets are overridden by the deblocking
	/// parameter offsets present in the slice headers of the slices referring
	/// to the PPS.
	pub tc_offset_div2: i8,
	/// When set, specifies that the scaling list data used for the pictures
	/// referring to the PPS are derived based on the scaling lists specified by
	/// the active SPS and the scaling lists specified by the PPS.
	/// pps_scaling_list_data_present_flag equal to 0 specifies that the scaling
	/// list data used for the pictures referring to the PPS are inferred to be
	/// equal to those specified by the active SPS.
	pub scaling_list_data_present_flag: bool,
	/// The scaling list data.
	pub scaling_list: ScalingLists,
	/// When set, specifies that the syntax structure
	/// ref_pic_lists_modification( ) is present in the slice segment header.
	/// When not set, specifies that the syntax structure
	/// ref_pic_lists_modification( ) is not present in the slice segment header
	pub lists_modification_present_flag: bool,
	/// log2_parallel_merge_level_minus2 plus 2 specifies the value of the
	/// variable Log2ParMrgLevel, which is used in the derivation process for
	/// luma motion vectors for merge mode as specified in clause 8.5.3.2.2 and
	/// the derivation process for spatial merging candidates as specified in
	/// clause 8.5.3.2.3.
	pub log2_parallel_merge_level_minus2: u8,
	/// When not set, specifies that no slice segment header extension syntax
	/// elements are present in the slice segment headers for coded pictures
	/// referring to the PPS. When set, specifies that slice segment header
	/// extension syntax elements are present in the slice segment headers for
	/// coded pictures referring to the PPS.
	pub slice_segment_header_extension_present_flag: bool,
	/// When set, specifies that the syntax elements pps_range_extension_flag,
	/// pps_multilayer_extension_flag, pps_3d_extension_flag,
	/// pps_scc_extension_flag, and pps_extension_4bits are present in the
	/// picture parameter set RBSP syntax structure. When not set, specifies
	/// that these syntax elements are not present.
	pub extension_present_flag: bool,
	/// When setspecifies that the pps_range_extension( ) syntax structure is
	/// present in the PPS RBSP syntax structure. When not set, specifies that
	/// this syntax structure is not present.
	pub range_extension_flag: bool,
	/// The range extension data.
	pub range_extension: PpsRangeExtension,

	pub scc_extension_flag: bool,
	/// The SCC extension data.
	pub scc_extension: PpsSccExtension,

	// Internal variables.
	/// Equivalent to QpBdOffsetY in the specification.
	pub qp_bd_offset_y: u32,

	/// The SPS referenced by this PPS.
	pub sps: Rc<Sps>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalingLists {
	/// plus 8 specifies the value of the variable `ScalingFactor[ 2 ][ matrixId
	/// ] [ 0 ][ 0 ]` for the scaling list for the 16x16 size.
	pub scaling_list_dc_coef_minus8_16x16: [i16; 6],
	/// plus 8 specifies the value of the variable `ScalingFactor[ 3 ][ matrixId
	/// ][ 0 ][ 0 ]` for the scaling list for the 32x32 size.
	pub scaling_list_dc_coef_minus8_32x32: [i16; 6],
	/// The 4x4 scaling list.
	pub scaling_list_4x4: [[u8; 16]; 6],
	/// The 8x8 scaling list.
	pub scaling_list_8x8: [[u8; 64]; 6],
	/// The 16x16 scaling list.
	pub scaling_list_16x16: [[u8; 64]; 6],
	/// The 32x32 scaling list.
	pub scaling_list_32x32: [[u8; 64]; 6],
}

impl Default for ScalingLists {
	fn default() -> Self {
		Self {
			scaling_list_dc_coef_minus8_16x16: Default::default(),
			scaling_list_dc_coef_minus8_32x32: Default::default(),
			scaling_list_4x4: Default::default(),
			scaling_list_8x8: [[0; 64]; 6],
			scaling_list_16x16: [[0; 64]; 6],
			scaling_list_32x32: [[0; 64]; 6],
		}
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefPicListModification {
	/// Whenset, indicates that reference picture list 0 is specified explicitly
	/// by a list of `list_entry_l0[ i ]` values.  When not set, indicates that
	/// reference picture list 0 is determined implicitly.
	pub ref_pic_list_modification_flag_l0: bool,
	/// `list_entry_l0[ i ]` specifies the index of the reference picture in
	/// RefPicListTemp0 to be placed at the current position of reference
	/// picture list 0.
	pub list_entry_l0: Vec<u32>,
	/// Whenset, indicates that reference picture list 1 is specified explicitly
	/// by a list of `list_entry_l1[ i ]` values.  When not set, indicates that
	/// reference picture list 1 is determined implicitly.
	pub ref_pic_list_modification_flag_l1: bool,
	/// `list_entry_l1[ i ]` specifies the index of the reference picture in
	/// RefPicListTemp1 to be placed at the current position of reference
	/// picture list 1.
	pub list_entry_l1: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PredWeightTable {
	/// The base 2 logarithm of the denominator for all luma weighting factors.
	pub luma_log2_weight_denom: u8,
	/// The difference of the base 2 logarithm of the denominator for all chroma
	/// weighting factors.
	pub delta_chroma_log2_weight_denom: i8,
	/// `luma_weight_l0_flag[ i ]` set specifies that weighting factors for the
	/// luma component of list 0 prediction using `RefPicList0[ i ]` are present.
	/// `luma_weight_l0_flag[ i ]` not set specifies that these weighting factors
	/// are not present.
	pub luma_weight_l0_flag: [bool; 15],
	/// `chroma_weight_l0_flag[ i ]` set specifies that weighting factors for the
	/// chroma prediction values of list 0 prediction using `RefPicList0[ i ]` are
	/// present. `chroma_weight_l0_flag[ i ]` not set specifies that these
	/// weighting factors are not present.
	pub chroma_weight_l0_flag: [bool; 15],
	/// `delta_luma_weight_l0[ i ]` is the difference of the weighting factor
	/// applied to the luma prediction value for list 0 prediction using
	/// `RefPicList0[ i ]`.
	pub delta_luma_weight_l0: [i8; 15],
	/// `luma_offset_l0[ i ]` is the additive offset applied to the luma
	/// prediction value for list 0 prediction using `RefPicList0[ i ]`.
	pub luma_offset_l0: [i8; 15],
	/// `delta_chroma_weight_l0[ i ][ j ]` is the difference of the weighting
	/// factor applied to the chroma prediction values for list 0 prediction
	/// using `RefPicList0[ i ]` with j equal to 0 for Cb and j equal to 1 for Cr.
	pub delta_chroma_weight_l0: [[i8; 2]; 15],
	/// `delta_chroma_offset_l0[ i ][ j ]` is the difference of the additive
	/// offset applied to the chroma prediction values for list 0 prediction
	/// using `RefPicList0[ i ]` with j equal to 0 for Cb and j equal to 1 for Cr.
	pub delta_chroma_offset_l0: [[i16; 2]; 15],

	// `luma_weight_l1_flag[ i ]`, `chroma_weight_l1_flag[ i ]`,
	// `delta_luma_weight_l1[ i ]`, `luma_offset_l1[ i ]`, delta_chroma_weight_l1[ i
	// `][ j ]` and `delta_chroma_offset_l1[ i ]`[ j ] have the same
	// `semanticsasluma_weight_l0_flag[ i ]`, `chroma_weight_l0_flag[ i ]`,
	// `delta_luma_weight_l0[ i ]`, `luma_offset_l0[ i ]`, `delta_chroma_weight_l0[ i
	// ][ j ]` and `delta_chroma_offset_l0[ i ][ j ]`, respectively, with `l0`, `L0`,
	// `list 0` and `List0` replaced by `l1`, `L1`, `list 1` and `List1`, respectively.
	pub luma_weight_l1_flag: [bool; 15],
	pub chroma_weight_l1_flag: [bool; 15],
	pub delta_luma_weight_l1: [i8; 15],
	pub luma_offset_l1: [i8; 15],

	pub delta_chroma_weight_l1: [[i8; 2]; 15],
	pub delta_chroma_offset_l1: [[i16; 2]; 15],

	// Calculated.
	/// Same as ChromaLog2WeightDenom in the specification.
	pub chroma_log2_weight_denom: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortTermRefPicSet {
	/// When set, specifies that the stRpsIdx-th candidate short-term RPS is
	/// predicted from another candidate short-term RPS, which is referred to as
	/// the source candidate short-term RPS.
	pub inter_ref_pic_set_prediction_flag: bool,
	/// delta_idx_minus1 plus 1 specifies the difference between the value of
	/// stRpsIdx and the index, into the list of the candidate short-term RPSs
	/// specified in the SPS, of the source candidate short-term RPS.
	pub delta_idx_minus1: u8,
	/// delta_rps_sign and abs_delta_rps_minus1 together specify the value of
	/// the variable deltaRps.
	pub delta_rps_sign: bool,
	/// delta_rps_sign and abs_delta_rps_minus1 together specify the value of
	/// the variable deltaRps.
	pub abs_delta_rps_minus1: u16,
	/// specifies the number of entries in the stRpsIdx-th candidate short-term
	/// RPS that have picture order count values less than the picture order
	/// count value of the current picture.
	pub num_negative_pics: u8,
	/// specifies the number of entries in the stRpsIdx-th candidate short-term
	/// RPS that have picture order count values greater than the picture order
	/// count value of the current picture.
	pub num_positive_pics: u8,
	/// Same as UsedByCurrPicS0 in the specification.
	pub used_by_curr_pic_s0: [bool; MAX_SHORT_TERM_REF_PIC_SETS],
	/// Same as UsedByCurrPicS1 in the specification.
	pub used_by_curr_pic_s1: [bool; MAX_SHORT_TERM_REF_PIC_SETS],
	/// Same as DeltaPocS0 in the specification.
	pub delta_poc_s0: [i32; MAX_SHORT_TERM_REF_PIC_SETS],
	/// Same as DeltaPocS1 in the specification.
	pub delta_poc_s1: [i32; MAX_SHORT_TERM_REF_PIC_SETS],
	/// Same as NumDeltaPocs in the specification.
	pub num_delta_pocs: u32,
}

impl Default for ShortTermRefPicSet {
	fn default() -> Self {
		Self {
			inter_ref_pic_set_prediction_flag: Default::default(),
			delta_idx_minus1: Default::default(),
			delta_rps_sign: Default::default(),
			abs_delta_rps_minus1: Default::default(),
			num_negative_pics: Default::default(),
			num_positive_pics: Default::default(),
			used_by_curr_pic_s0: [false; MAX_SHORT_TERM_REF_PIC_SETS],
			used_by_curr_pic_s1: [false; MAX_SHORT_TERM_REF_PIC_SETS],
			delta_poc_s0: [0; MAX_SHORT_TERM_REF_PIC_SETS],
			delta_poc_s1: [0; MAX_SHORT_TERM_REF_PIC_SETS],
			num_delta_pocs: Default::default(),
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// See table 7-7 in the specification.
pub enum SliceType {
	B = 0,
	P = 1,
	I = 2,
}

impl TryFrom<u32> for SliceType {
	type Error = String;

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		match value {
			0 => Ok(SliceType::B),
			1 => Ok(SliceType::P),
			2 => Ok(SliceType::I),
			_ => Err(format!("Invalid SliceType {}", value)),
		}
	}
}

impl SliceType {
	/// Whether this is a P slice. See table 7-7 in the specification.
	pub fn is_p(&self) -> bool {
		matches!(self, SliceType::P)
	}

	/// Whether this is a B slice. See table 7-7 in the specification.
	pub fn is_b(&self) -> bool {
		matches!(self, SliceType::B)
	}

	/// Whether this is an I slice. See table 7-7 in the specification.
	pub fn is_i(&self) -> bool {
		matches!(self, SliceType::I)
	}
}

impl Default for SliceType {
	fn default() -> Self {
		Self::P
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceHeader {
	/// When set, specifies that the slice segment is the first slice segment of
	/// the picture in decoding order. When not set, specifies that the slice
	/// segment is not the first slice segment of the picture in decoding order.
	pub first_slice_segment_in_pic_flag: bool,
	/// Affects the output of previously-decoded pictures in the decoded picture
	/// buffer after the decoding of an IDR or a BLA picture that is not the
	/// first picture in the bitstream as specified in Annex C.
	pub no_output_of_prior_pics_flag: bool,
	/// Specifies the value of pps_pic_parameter_set_id for the PPS in use.
	pub pic_parameter_set_id: u8,
	/// When set, specifies that the value of each slice segment header syntax
	/// element that is not present is inferred to be equal to the value of the
	/// corresponding slice segment header syntax element in the slice header.
	pub dependent_slice_segment_flag: bool,
	/// Specifies the address of the first CTB in the slice segment, in CTB
	/// raster scan of a picture.
	pub segment_address: u32,
	/// Specifies the coding type of the slice according to Table 7-7.
	pub type_: SliceType,
	/// Affects the decoded picture output and removal processes as specified in
	/// Annex C.
	pub pic_output_flag: bool,
	/// Specifies the colour plane associated with the current slice RBSP when
	/// separate_colour_plane_flag is set. The value of colour_plane_id shall be
	/// in the range of 0 to 2, inclusive. colour_plane_id values 0, 1 and 2
	/// correspond to the Y, Cb and Cr planes, respectively.
	pub colour_plane_id: u8,
	/// Specifies the picture order count modulo MaxPicOrderCntLsb for the
	/// current picture. The length of the slice_pic_order_cnt_lsb syntax
	/// element is log2_max_pic_order_cnt_lsb_minus4 + 4 bits.
	pub pic_order_cnt_lsb: u16,
	/// When set, specifies that the short-term RPS of the current picture is
	/// derived based on one of the st_ref_pic_set( ) syntax structures in the
	/// active SPS that is identified by the syntax element
	/// short_term_ref_pic_set_idx in the slice header.  When not set, specifies
	/// that the short-term RPS of the current picture is derived based on the
	/// st_ref_pic_set( ) syntax structure that is directly included in the
	/// slice headers of the current picture.
	pub short_term_ref_pic_set_sps_flag: bool,
	/// The st_ref_pic_set() data.
	pub short_term_ref_pic_set: ShortTermRefPicSet,
	/// Specifies the index, into the list of the st_ref_pic_set( ) syntax
	/// structures included in the active SPS, of the st_ref_pic_set( ) syntax
	/// structure that is used for derivation of the short-term RPS of the
	/// current picture.
	pub short_term_ref_pic_set_idx: u8,
	/// Specifies the number of entries in the long-term RPS of the current
	/// picture that are derived based on the candidate long-term reference
	/// pictures specified in the active SPS.
	pub num_long_term_sps: u8,
	/// Specifies the number of entries in the long-term RPS of the current
	/// picture that are directly signalled in the slice header.
	pub num_long_term_pics: u8,
	/// `lt_idx_sps[ i ]` specifies an index, into the list of candidate long-term
	/// reference pictures specified in the active SPS, of the i-th entry in the
	/// long-term RPS of the current picture.
	pub lt_idx_sps: [u8; 16],
	/// Same as PocLsbLt in the specification.
	pub poc_lsb_lt: [u32; 16],
	/// Same as UsedByCurrPicLt in the specification.
	pub used_by_curr_pic_lt: [bool; 16],
	/// When set, specifies that that `delta_poc_msb_cycle_lt[i]` is present.
	pub delta_poc_msb_present_flag: [bool; 16],
	/// Same as DeltaPocMsbCycleLt in the specification.
	pub delta_poc_msb_cycle_lt: [u32; 16],
	/// Specifies whether temporal motion vector predictors can be used for
	/// inter prediction. If slice_temporal_mvp_enabled_flag is not set, the
	/// syntax elements of the current picture shall be constrained such that no
	/// temporal motion vector predictor is used in decoding of the current
	/// picture. Otherwise (slice_temporal_mvp_enabled_flag is set), temporal
	/// motion vector predictors may be used in decoding of the current picture.
	pub temporal_mvp_enabled_flag: bool,
	/// When set, specifies that SAO is enabled for the luma component in the
	/// current slice; slice_sao_luma_flag not set specifies that SAO is
	/// disabled for the luma component in the current slice.
	pub sao_luma_flag: bool,
	/// When set, specifies that SAO is enabled for the chroma component in the
	/// current slice; When not set, specifies that SAO is disabled for the
	/// chroma component in the current slice.
	pub sao_chroma_flag: bool,
	/// When set, specifies that the syntax element num_ref_idx_l0_active_minus1
	/// is present for P and B slices and that the syntax element
	/// num_ref_idx_l1_active_minus1 is present for B slices. When not set,
	/// specifies that the syntax elements num_ref_idx_l0_active_minus1 and
	/// num_ref_idx_l1_active_minus1 are not present.
	pub num_ref_idx_active_override_flag: bool,
	/// Specifies the maximum reference index for
	/// reference picture list 0 that may be used to decode the slice.
	pub num_ref_idx_l0_active_minus1: u8,
	/// Specifies the maximum reference index for reference picture list 1 that
	/// may be used to decode the slice.
	pub num_ref_idx_l1_active_minus1: u8,
	/// The RefPicListModification data.
	pub ref_pic_list_modification: RefPicListModification,
	/// When set, indicates that the mvd_coding( x0, y0, 1 ) syntax structure is
	/// not parsed and `MvdL1[ x0 ]`[ y0 `][ compIdx ]` is set equal to 0 for
	/// compIdx = 0..1. When not set, indicates that the mvd_coding( x0, y0, 1 )
	/// syntax structure is parsed.
	pub mvd_l1_zero_flag: bool,
	/// Specifies the method for determining the initialization table used in
	/// the initialization process for context variables.
	pub cabac_init_flag: bool,
	/// When set, specifies that the collocated picture used for temporal motion
	/// vector prediction is derived from reference picture list 0.  When not
	/// set, specifies that the collocated picture used for temporal motion
	/// vector prediction is derived from reference picture list 1.
	pub collocated_from_l0_flag: bool,
	/// Specifies the reference index of the collocated picture used for
	/// temporal motion vector prediction.
	pub collocated_ref_idx: u8,
	/// The PredWeightTable data.
	pub pred_weight_table: PredWeightTable,
	/// Specifies the maximum number of merging motion vector prediction (MVP)
	/// candidates supported in the slice subtracted from 5.
	pub five_minus_max_num_merge_cand: u8,
	/// Specifies that the resolution of motion vectors for inter prediction in
	/// the current slice is integer. When not set, specifies
	/// that the resolution of motion vectors for inter prediction in the
	/// current slice that refer to pictures other than the current picture is
	/// fractional with quarter-sample precision in units of luma samples.
	pub use_integer_mv_flag: bool,
	/// Specifies the initial value of QpY to be used for the coding blocks in
	/// the slice until modified by the value of CuQpDeltaVal in the coding unit
	/// layer.
	pub qp_delta: i8,
	/// Specifies a difference to be added to the value of pps_cb_qp_offset when
	/// determining the value of the Qp′Cb quantization parameter.
	pub cb_qp_offset: i8,
	/// Specifies a difference to be added to the value of pps_cb_qr_offset when
	/// determining the value of the Qp′Cr quantization parameter.
	pub cr_qp_offset: i8,
	/// Specifies offsets to the quantization parameter values qP derived in
	/// clause 8.6.2 for luma, Cb, and Cr components, respectively.
	pub slice_act_y_qp_offset: i8,
	/// Specifies offsets to the quantization parameter values qP derived in
	/// clause 8.6.2 for luma, Cb, and Cr components, respectively.
	pub slice_act_cb_qp_offset: i8,
	/// Specifies offsets to the quantization parameter values qP derived in
	/// clause 8.6.2 for luma, Cb, and Cr components, respectively.
	pub slice_act_cr_qp_offset: i8,
	/// When set, specifies that the cu_chroma_qp_offset_flag may be present in
	/// the transform unit syntax. When not set, specifies that the
	/// cu_chroma_qp_offset_flag is not present in the transform unit syntax.
	pub cu_chroma_qp_offset_enabled_flag: bool,
	/// When set, specifies that deblocking parameters are present in the slice
	/// header. When not set, specifies that deblocking parameters are not
	/// present in the slice header.
	pub deblocking_filter_override_flag: bool,
	/// When set, specifies that the operation of the deblocking filter is not
	/// applied for the current slice. When not set, specifies that the
	/// operation of the deblocking filter is applied for the current slice.
	pub deblocking_filter_disabled_flag: bool,
	/// Specifies the deblocking parameter offsets for β and tC (divided by 2)
	/// for the current slice.
	pub beta_offset_div2: i8,
	/// Specifies the deblocking parameter offsets for β and tC (divided by 2)
	/// for the current slice.
	pub tc_offset_div2: i8,
	/// When set, specifies that in-loop filtering operations may be performed
	/// across the left and upper boundaries of the current slice.  When not
	/// set, specifies that in-loop operations are not performed across left and
	/// upper boundaries of the current slice. The in-loop filtering operations
	/// include the deblocking filter and sample adaptive offset filter.
	pub loop_filter_across_slices_enabled_flag: bool,
	/// Specifies the number of `entry_point_offset_minus1[ i ]` syntax elements
	/// in the slice header.
	pub num_entry_point_offsets: u32,
	/// offset_len_minus1 plus 1 specifies the length, in bits, of the
	/// `entry_point_offset_minus1[ i ]` syntax elements.
	pub offset_len_minus1: u8,
	/// `entry_point_offset_minus1[ i ]` plus 1 specifies the i-th entry point
	/// offset in bytes, and is represented by offset_len_minus1 plus 1 bits.
	/// The slice segment data that follow the slice segment header consists of
	/// num_entry_point_offsets + 1 subsets, with subset index values ranging
	/// from 0 to num_entry_point_offsets, inclusive. See the specification for
	/// more details.
	pub entry_point_offset_minus1: Vec<u32>,
	/// Same as NumPicTotalCurr in the specification.
	pub num_pic_total_curr: u32,
	// Size of slice_header() in bits.
	pub header_bit_size: u32,
	// Number of emulation prevention bytes (EPB) in this slice_header().
	pub n_emulation_prevention_bytes: u32,
	/// Same as CurrRpsIdx in the specification.
	pub curr_rps_idx: u8,
	/// Number of bits taken by st_ref_pic_set minus Emulation Prevention Bytes.
	pub st_rps_bits: u32,
}

impl Default for SliceHeader {
	fn default() -> Self {
		Self {
			first_slice_segment_in_pic_flag: Default::default(),
			no_output_of_prior_pics_flag: Default::default(),
			pic_parameter_set_id: Default::default(),
			dependent_slice_segment_flag: Default::default(),
			segment_address: Default::default(),
			type_: Default::default(),
			pic_output_flag: true,
			colour_plane_id: Default::default(),
			pic_order_cnt_lsb: Default::default(),
			short_term_ref_pic_set_sps_flag: Default::default(),
			short_term_ref_pic_set: Default::default(),
			short_term_ref_pic_set_idx: Default::default(),
			num_long_term_sps: Default::default(),
			num_long_term_pics: Default::default(),
			lt_idx_sps: Default::default(),
			poc_lsb_lt: Default::default(),
			used_by_curr_pic_lt: Default::default(),
			delta_poc_msb_present_flag: Default::default(),
			delta_poc_msb_cycle_lt: Default::default(),
			temporal_mvp_enabled_flag: Default::default(),
			sao_luma_flag: Default::default(),
			sao_chroma_flag: Default::default(),
			num_ref_idx_active_override_flag: Default::default(),
			num_ref_idx_l0_active_minus1: Default::default(),
			num_ref_idx_l1_active_minus1: Default::default(),
			ref_pic_list_modification: Default::default(),
			mvd_l1_zero_flag: Default::default(),
			cabac_init_flag: Default::default(),
			collocated_from_l0_flag: true,
			collocated_ref_idx: Default::default(),
			pred_weight_table: Default::default(),
			five_minus_max_num_merge_cand: Default::default(),
			use_integer_mv_flag: Default::default(),
			qp_delta: Default::default(),
			cb_qp_offset: Default::default(),
			cr_qp_offset: Default::default(),
			slice_act_y_qp_offset: Default::default(),
			slice_act_cb_qp_offset: Default::default(),
			slice_act_cr_qp_offset: Default::default(),
			cu_chroma_qp_offset_enabled_flag: Default::default(),
			deblocking_filter_override_flag: Default::default(),
			deblocking_filter_disabled_flag: Default::default(),
			beta_offset_div2: Default::default(),
			tc_offset_div2: Default::default(),
			loop_filter_across_slices_enabled_flag: Default::default(),
			num_entry_point_offsets: Default::default(),
			offset_len_minus1: Default::default(),
			entry_point_offset_minus1: Default::default(),
			num_pic_total_curr: Default::default(),
			header_bit_size: Default::default(),
			n_emulation_prevention_bytes: Default::default(),
			curr_rps_idx: Default::default(),
			st_rps_bits: Default::default(),
		}
	}
}

/// A H265 slice. An integer number of macroblocks or macroblock pairs ordered
/// consecutively in the raster scan within a particular slice group
pub struct Slice<'a> {
	/// The slice header.
	pub header: SliceHeader,
	/// The NAL unit backing this slice.
	pub nalu: Nalu<'a>,
}

impl<'a> Slice<'a> {
	/// Sets the header for dependent slices by copying from an independent
	/// slice.
	pub fn replace_header(&mut self, header: SliceHeader) -> Result<(), String> {
		if !self.header.dependent_slice_segment_flag {
			Err("Replacing the slice header is only possible for dependent slices".into())
		} else {
			let first_slice_segment_in_pic_flag = self.header.first_slice_segment_in_pic_flag;
			let no_output_of_prior_pics_flag = self.header.no_output_of_prior_pics_flag;
			let pic_parameter_set_id = self.header.pic_parameter_set_id;
			let dependent_slice_segment_flag = self.header.dependent_slice_segment_flag;
			let segment_address = self.header.segment_address;

			let offset_len_minus1 = self.header.offset_len_minus1;
			let entry_point_offset_minus1 = std::mem::take(&mut self.header.entry_point_offset_minus1);
			let num_pic_total_curr = self.header.num_pic_total_curr;
			let header_bit_size = self.header.header_bit_size;
			let n_emulation_prevention_bytes = self.header.n_emulation_prevention_bytes;
			let curr_rps_idx = self.header.curr_rps_idx;
			let st_rps_bits = self.header.st_rps_bits;

			self.header = header;

			self.header.first_slice_segment_in_pic_flag = first_slice_segment_in_pic_flag;
			self.header.no_output_of_prior_pics_flag = no_output_of_prior_pics_flag;
			self.header.pic_parameter_set_id = pic_parameter_set_id;
			self.header.dependent_slice_segment_flag = dependent_slice_segment_flag;
			self.header.segment_address = segment_address;
			self.header.offset_len_minus1 = offset_len_minus1;
			self.header.entry_point_offset_minus1 = entry_point_offset_minus1;
			self.header.num_pic_total_curr = num_pic_total_curr;
			self.header.header_bit_size = header_bit_size;
			self.header.n_emulation_prevention_bytes = n_emulation_prevention_bytes;
			self.header.curr_rps_idx = curr_rps_idx;
			self.header.st_rps_bits = st_rps_bits;

			Ok(())
		}
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SublayerHrdParameters {
	// NOTE: The value of CpbCnt is cpb_cnt_minus1[i] + 1, and cpb_cnt_minus1
	// ranges from 0..=31
	/// `bit_rate_value_minus1[ i ]` (together with bit_rate_scale) specifies the
	/// maximum input bit rate for the i-th CPB when the CPB operates at the
	/// access unit level
	pub bit_rate_value_minus1: [u32; 32],
	/// `cpb_size_value_minus1[ i ]` is used together with cpb_size_scale to
	/// specify the i-th CPB size when the CPB operates at the access unit
	/// level.
	pub cpb_size_value_minus1: [u32; 32],
	/// `cpb_size_du_value_minus1[ i ]` is used together with cpb_size_du_scale to
	/// specify the i-th CPB size when the CPB operates at sub-picture level.
	pub cpb_size_du_value_minus1: [u32; 32],
	/// `bit_rate_du_value_minus1[ i ]` (together with bit_rate_scale) specifies
	/// the maximum input bit rate for the i-th CPB when the CPB operates at the
	/// sub-picture level.
	pub bit_rate_du_value_minus1: [u32; 32],
	/// `cbr_flag[ i ]` not set specifies that to decode this CVS by the HRD using
	/// the i-th CPB specification.
	pub cbr_flag: [bool; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HrdParams {
	/// When set, specifies that NAL HRD parameters (pertaining to the Type II
	/// bitstream conformance point) are present in the hrd_parameters( ) syntax
	/// structure. When not set, specifies that NAL HRD parameters are not
	/// present in the hrd_parameters( ) syntax structure.
	pub nal_hrd_parameters_present_flag: bool,
	/// When set, specifies that VCL HRD parameters (pertaining to the Type I
	/// bitstream conformance point) are present in the hrd_parameters( ) syntax
	/// structure. When not set, specifies that VCL HRD parameters are not
	/// present in the hrd_parameters( ) syntax structure.
	pub vcl_hrd_parameters_present_flag: bool,
	/// When set, specifies that sub-picture level HRD parameters are present
	/// and the HRD may operate at access unit level or sub-picture level. When
	/// not set, specifies that sub-picture level HRD parameters are not present
	/// and the HRD operates at access unit level.
	pub sub_pic_hrd_params_present_flag: bool,
	/// Used to specify the clock sub-tick. A clock sub-tick is the minimum
	/// interval of time that can be represented in the coded data when
	/// sub_pic_hrd_params_present_flag is set.
	pub tick_divisor_minus2: u8,
	/// du_cpb_removal_delay_increment_length_minus1 plus 1 specifies the
	/// length, in bits, of the `du_cpb_removal_delay_increment_minus1[ i ]` and
	/// du_common_cpb_removal_delay_increment_minus1 syntax elements of the
	/// picture timing SEI message and the du_spt_cpb_removal_delay_increment
	/// syntax element in the decoding unit information SEI message.
	pub du_cpb_removal_delay_increment_length_minus1: u8,
	/// When set, specifies that sub-picture level CPB removal delay parameters
	/// are present in picture timing SEI messages and no decoding unit
	/// information SEI message is available (in the CVS or provided through
	/// external means not specified in this Specification).  When not set,
	/// specifies that sub-picture level CPB removal delay parameters are
	/// present in decoding unit information SEI messages and picture timing SEI
	/// messages do not include sub-picture level CPB removal delay parameters.
	pub sub_pic_cpb_params_in_pic_timing_sei_flag: bool,
	/// dpb_output_delay_du_length_minus1 plus 1 specifies the length, in bits,
	/// of the pic_dpb_output_du_delay syntax element in the picture timing SEI
	/// message and the pic_spt_dpb_output_du_delay syntax element in the
	/// decoding unit information SEI message.
	pub dpb_output_delay_du_length_minus1: u8,
	/// Together with `bit_rate_value_minus1[ i ]`, specifies the maximum input
	/// bit rate of the i-th CPB.
	pub bit_rate_scale: u8,
	/// Together with `cpb_size_du_value_minus1[ i ]`, specifies the CPB size of
	/// the i-th CPB when the CPB operates at sub-picture level.
	pub cpb_size_scale: u8,
	/// Together with `cpb_size_du_value_minus1[ i ]`, specifies the CPB size of
	/// the i-th CPB when the CPB operates at sub-picture level.
	pub cpb_size_du_scale: u8,
	/// initial_cpb_removal_delay_length_minus1 plus 1 specifies the length, in
	/// bits, of the `nal_initial_cpb_removal_delay[ i ]`,
	/// `nal_initial_cpb_removal_offset[ i ]`, `vcl_initial_cpb_removal_delay[ i ]`
	/// and `vcl_initial_cpb_removal_offset[ i ]` syntax elements of the buffering
	/// period SEI message.
	pub initial_cpb_removal_delay_length_minus1: u8,
	/// au_cpb_removal_delay_length_minus1 plus 1 specifies the length, in bits,
	/// of the cpb_delay_offset syntax element in the buffering period SEI
	/// message and the au_cpb_removal_delay_minus1 syntax element in the
	/// picture timing SEI message.
	pub au_cpb_removal_delay_length_minus1: u8,
	/// dpb_output_delay_length_minus1 plus 1 specifies the length, in bits, of
	/// the dpb_delay_offset syntax element in the buffering period SEI message
	/// and the pic_dpb_output_delay syntax element in the picture timing SEI
	/// message.
	pub dpb_output_delay_length_minus1: u8,
	/// `fixed_pic_rate_general_flag[ i ]` set indicates that, when HighestTid is
	/// equal to i, the temporal distance between the HRD output times of
	/// consecutive pictures in output order is constrained as specified in the
	/// specification. `fixed_pic_rate_general_flag[ i ]` not set indicates that
	/// this constraint may not apply.
	pub fixed_pic_rate_general_flag: [bool; 7],
	/// `fixed_pic_rate_within_cvs_flag[ i ]` set indicates that, when HighestTid
	/// is equal to i, the temporal distance between the HRD output times of
	/// consecutive pictures in output order is constrained as specified in the
	/// specification. `fixed_pic_rate_within_cvs_flag[ i ]` not set indicates
	/// that this constraint may not apply.
	pub fixed_pic_rate_within_cvs_flag: [bool; 7],
	/// `elemental_duration_in_tc_minus1[ i ]` plus 1 (when present) specifies,
	/// when HighestTid is equal to i, the temporal distance, in clock ticks,
	/// between the elemental units that specify the HRD output times of
	/// consecutive pictures in output order as specified in the specification.
	pub elemental_duration_in_tc_minus1: [u32; 7],
	/// `low_delay_hrd_flag[ i ]` specifies the HRD operational mode, when
	/// HighestTid is equal to i, as specified in Annex C or clause F.13.
	pub low_delay_hrd_flag: [bool; 7],
	/// `cpb_cnt_minus1[ i ]` plus 1 specifies the number of alternative CPB
	/// specifications in the bitstream of the CVS when HighestTid is equal to
	/// i.
	pub cpb_cnt_minus1: [u32; 7],
	/// The NAL HRD data.
	pub nal_hrd: [SublayerHrdParameters; 7],
	/// The VCL HRD data.
	pub vcl_hrd: [SublayerHrdParameters; 7],
}

impl Default for HrdParams {
	fn default() -> Self {
		Self {
			initial_cpb_removal_delay_length_minus1: 23,
			au_cpb_removal_delay_length_minus1: 23,
			dpb_output_delay_du_length_minus1: 23,
			nal_hrd_parameters_present_flag: Default::default(),
			vcl_hrd_parameters_present_flag: Default::default(),
			sub_pic_hrd_params_present_flag: Default::default(),
			tick_divisor_minus2: Default::default(),
			du_cpb_removal_delay_increment_length_minus1: Default::default(),
			sub_pic_cpb_params_in_pic_timing_sei_flag: Default::default(),
			bit_rate_scale: Default::default(),
			cpb_size_scale: Default::default(),
			cpb_size_du_scale: Default::default(),
			dpb_output_delay_length_minus1: Default::default(),
			fixed_pic_rate_general_flag: Default::default(),
			fixed_pic_rate_within_cvs_flag: Default::default(),
			elemental_duration_in_tc_minus1: Default::default(),
			low_delay_hrd_flag: Default::default(),
			cpb_cnt_minus1: Default::default(),
			nal_hrd: Default::default(),
			vcl_hrd: Default::default(),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VuiParams {
	/// When set, specifies that aspect_ratio_idc is present.  When not set,
	/// specifies that aspect_ratio_idc is not present.
	pub aspect_ratio_info_present_flag: bool,
	/// Specifies the value of the sample aspect ratio of the luma samples.
	pub aspect_ratio_idc: u32,
	/// Indicates the horizontal size of the sample aspect ratio (in arbitrary
	/// units).
	pub sar_width: u32,
	/// Indicates the vertical size of the sample aspect ratio (in arbitrary
	/// units).
	pub sar_height: u32,
	/// When set, specifies that the overscan_appropriate_flag is present. When
	/// not set, the preferred display method for the video signal is
	/// unspecified.
	pub overscan_info_present_flag: bool,
	/// When set indicates that the cropped decoded pictures output are suitable
	/// for display using overscan. When not set, indicates that the cropped
	/// decoded pictures output contain visually important information in the
	/// entire region out to the edges of the conformance cropping window of the
	/// picture, such that the cropped decoded pictures output should not be
	/// displayed using overscan.
	pub overscan_appropriate_flag: bool,
	/// When set, specifies that video_format, video_full_range_flag and
	/// colour_description_present_flag are present.  When not set, specify that
	/// video_format, video_full_range_flag and colour_description_present_flag
	/// are not present.
	pub video_signal_type_present_flag: bool,
	/// Indicates the representation of the pictures as specified in Table E.2,
	/// before being coded in accordance with this Specification.
	pub video_format: u8,
	/// Indicates the black level and range of the luma and chroma signals as
	/// derived from E′Y, E′PB, and E′PR or E′R, E′G, and E′B real-valued
	/// component signals.
	pub video_full_range_flag: bool,
	/// When set, specifies that colour_primaries, transfer_characteristics, and
	/// matrix_coeffs are present. When not set, specifies that
	/// colour_primaries, transfer_characteristics, and matrix_coeffs are not
	/// present.
	pub colour_description_present_flag: bool,
	/// Indicates the chromaticity coordinates of the source primaries as
	/// specified in Table E.3 in terms of the CIE 1931 definition of x and y as
	/// specified in ISO 11664-1.
	pub colour_primaries: u32,
	/// See table E.4 in the specification.
	pub transfer_characteristics: u32,
	/// Describes the matrix coefficients used in deriving luma and chroma
	/// signals from the green, blue, and red, or Y, Z, and X primaries, as
	/// specified in Table E.5.
	pub matrix_coeffs: u32,
	/// When true, specifies that chroma_sample_loc_type_top_field and
	/// chroma_sample_loc_type_bottom_field are present. When false, specifies
	/// that chroma_sample_loc_type_top_field and
	/// chroma_sample_loc_type_bottom_field are not present.
	pub chroma_loc_info_present_flag: bool,
	/// See the specification for more details.
	pub chroma_sample_loc_type_top_field: u32,
	/// See the specification for more details.
	pub chroma_sample_loc_type_bottom_field: u32,
	/// When true, indicates that the value of all decoded chroma samples is
	/// equal to 1 << ( BitDepthC − 1 ). When false, provides no indication of
	/// decoded chroma sample values.
	pub neutral_chroma_indication_flag: bool,
	/// When true, indicates that the CVS conveys pictures that represent
	/// fields, and specifies that a picture timing SEI message shall be present
	/// in every access unit of the current CVS. When false, indicates that the
	/// CVS conveys pictures that represent frames and that a picture timing SEI
	/// message may or may not be present in any access unit of the current CVS.
	pub field_seq_flag: bool,
	/// When true, specifies that picture timing SEI messages are present for
	/// every picture and include the pic_struct, source_scan_type and
	/// duplicate_flag syntax elements. When false, specifies that the
	/// pic_struct syntax element is not present in picture timing SEI messages.
	pub frame_field_info_present_flag: bool,
	/// When true, indicates that the default display window parameters follow
	/// next in the VUI. When false, indicates that the default display window
	/// parameters are not present.
	pub default_display_window_flag: bool,
	/// Specifies the samples of the pictures in the CVS that are within the
	/// default display window, in terms of a rectangular region specified in
	/// picture coordinates for display.
	pub def_disp_win_left_offset: u32,
	/// Specifies the samples of the pictures in the CVS that are within the
	/// default display window, in terms of a rectangular region specified in
	/// picture coordinates for display.
	pub def_disp_win_right_offset: u32,
	/// Specifies the samples of the pictures in the CVS that are within the
	/// default display window, in terms of a rectangular region specified in
	/// picture coordinates for display.
	pub def_disp_win_top_offset: u32,
	/// Specifies the samples of the pictures in the CVS that are within the
	/// default display window, in terms of a rectangular region specified in
	/// picture coordinates for display.
	pub def_disp_win_bottom_offset: u32,
	/// When set, specifies that vui_num_units_in_tick, vui_time_scale,
	/// vui_poc_proportional_to_timing_flag and vui_hrd_parameters_present_flag
	/// are present in the vui_parameters( ) syntax structure.  When not set,
	/// specifies that vui_num_units_in_tick, vui_time_scale,
	/// vui_poc_proportional_to_timing_flag and vui_hrd_parameters_present_flag
	/// are not present in the vui_parameters( ) syntax structure
	pub timing_info_present_flag: bool,
	/// The number of time units of a clock operating at the frequency
	/// vui_time_scale Hz that corresponds to one increment (called a clock
	/// tick) of a clock tick counter.
	pub num_units_in_tick: u32,
	/// Is the number of time units that pass in one second. For example, a time
	/// coordinate system that measures time using a 27 MHz clock has a
	/// vui_time_scale of 27 000 000.
	pub time_scale: u32,
	/// When set, indicates that the picture order count value for each picture
	/// in the CVS that is not the first picture in the CVS, in decoding order,
	/// is proportional to the output time of the picture relative to the output
	/// time of the first picture in the CVS.  When not set, indicates that the
	/// picture order count value for each picture in the CVS that is not the
	/// first picture in the CVS, in decoding order, may or may not be
	/// proportional to the output time of the picture relative to the output
	/// time of the first picture in the CVS.
	pub poc_proportional_to_timing_flag: bool,
	/// vui_num_ticks_poc_diff_one_minus1 plus 1 specifies the number of clock
	/// ticks corresponding to a difference of picture order count values equal
	/// to 1.
	pub num_ticks_poc_diff_one_minus1: u32,
	/// When set, specifies that the syntax structure hrd_parameters( ) is
	/// present in the vui_parameters( ) syntax structure.  When not set,
	/// specifies that the syntax structure hrd_parameters( ) is not present in
	/// the vui_parameters( ) syntax structure.
	pub hrd_parameters_present_flag: bool,
	/// The hrd_parameters() data.
	pub hrd: HrdParams,
	/// When set, specifies that the bitstream restriction parameters for the
	/// CVS are present. When not set, specifies that the bitstream restriction
	/// parameters for the CVS are not present.
	pub bitstream_restriction_flag: bool,
	/// When set, indicates that each PPS that is active in the CVS has the same
	/// value of the syntax elements num_tile_columns_minus1,
	/// num_tile_rows_minus1, uniform_spacing_flag, `column_width_minus1[ i ]`,
	/// `row_height_minus1[ i ]` and loop_filter_across_tiles_enabled_flag, when
	/// present. When not set, indicates that tiles syntax elements in different
	/// PPSs may or may not have the same value
	pub tiles_fixed_structure_flag: bool,
	/// When not set, indicates that no sample outside the picture boundaries
	/// and no sample at a fractional sample position for which the sample value
	/// is derived using one or more samples outside the picture boundaries is
	/// used for inter prediction of any sample.  When set, indicates that one
	/// or more samples outside the picture boundaries may be used in inter
	/// prediction.
	pub motion_vectors_over_pic_boundaries_flag: bool,
	/// When set, indicates that all P and B slices (when present) that belong
	/// to the same picture have an identical reference picture list 0 and that
	/// all B slices (when present) that belong to the same picture have an
	/// identical reference picture list 1.
	pub restricted_ref_pic_lists_flag: bool,
	/// When not equal to 0, establishes a bound on the maximum possible size of
	/// distinct coded spatial segmentation regions in the pictures of the CVS.
	pub min_spatial_segmentation_idc: u32,
	/// Indicates a number of bytes not exceeded by the sum of the sizes of the
	/// VCL NAL units associated with any coded picture in the CVS.
	pub max_bytes_per_pic_denom: u32,
	/// Indicates an upper bound for the number of coded bits of coding_unit( )
	/// data for anycoding block in any picture of the CVS.
	pub max_bits_per_min_cu_denom: u32,
	/// Indicate the maximum absolute value of a decoded horizontal and vertical
	/// motion vector component, respectively, in quarter luma sample units, for
	/// all pictures in the CVS.
	pub log2_max_mv_length_horizontal: u32,
	/// Indicate the maximum absolute value of a decoded horizontal and vertical
	/// motion vector component, respectively, in quarter luma sample units, for
	/// all pictures in the CVS.
	pub log2_max_mv_length_vertical: u32,
}

impl Default for VuiParams {
	fn default() -> Self {
		Self {
			aspect_ratio_info_present_flag: Default::default(),
			aspect_ratio_idc: Default::default(),
			sar_width: Default::default(),
			sar_height: Default::default(),
			overscan_info_present_flag: Default::default(),
			overscan_appropriate_flag: Default::default(),
			video_signal_type_present_flag: Default::default(),
			video_format: 5,
			video_full_range_flag: Default::default(),
			colour_description_present_flag: Default::default(),
			colour_primaries: 2,
			transfer_characteristics: 2,
			matrix_coeffs: 2,
			chroma_loc_info_present_flag: Default::default(),
			chroma_sample_loc_type_top_field: Default::default(),
			chroma_sample_loc_type_bottom_field: Default::default(),
			neutral_chroma_indication_flag: Default::default(),
			field_seq_flag: Default::default(),
			frame_field_info_present_flag: Default::default(),
			default_display_window_flag: Default::default(),
			def_disp_win_left_offset: Default::default(),
			def_disp_win_right_offset: Default::default(),
			def_disp_win_top_offset: Default::default(),
			def_disp_win_bottom_offset: Default::default(),
			timing_info_present_flag: Default::default(),
			num_units_in_tick: Default::default(),
			time_scale: Default::default(),
			poc_proportional_to_timing_flag: Default::default(),
			num_ticks_poc_diff_one_minus1: Default::default(),
			hrd_parameters_present_flag: Default::default(),
			hrd: Default::default(),
			bitstream_restriction_flag: Default::default(),
			tiles_fixed_structure_flag: Default::default(),
			motion_vectors_over_pic_boundaries_flag: true,
			restricted_ref_pic_lists_flag: Default::default(),
			min_spatial_segmentation_idc: Default::default(),
			max_bytes_per_pic_denom: 2,
			max_bits_per_min_cu_denom: 1,
			log2_max_mv_length_horizontal: 15,
			log2_max_mv_length_vertical: 15,
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct Parser {
	active_vpses: BTreeMap<u8, Rc<Vps>>,
	active_spses: BTreeMap<u8, Rc<Sps>>,
	active_ppses: BTreeMap<u8, Rc<Pps>>,
}

impl Parser {
	/// Parse a VPS NALU.
	pub fn parse_vps(&mut self, nalu: &Nalu) -> Result<&Vps, String> {
		if !matches!(nalu.header.type_, NaluType::VpsNut) {
			return Err(format!(
				"Invalid NALU type, expected {:?}, got {:?}",
				NaluType::VpsNut,
				nalu.header.type_
			));
		}

		let data = nalu.as_ref();
		let header = &nalu.header;
		let hdr_len = header.len();
		// Skip the header
		let mut r = BitReader::new(&data[hdr_len..], true);

		let mut vps = Vps {
			video_parameter_set_id: r.read_bits(4)?,
			base_layer_internal_flag: r.read_bit()?,
			base_layer_available_flag: r.read_bit()?,
			max_layers_minus1: r.read_bits(6)?,
			max_sub_layers_minus1: r.read_bits(3)?,
			temporal_id_nesting_flag: r.read_bit()?,
			..Default::default()
		};

		r.skip_bits(16)?; // vps_reserved_0xffff_16bits

		let ptl = &mut vps.profile_tier_level;
		Self::parse_profile_tier_level(ptl, &mut r, true, vps.max_sub_layers_minus1)?;

		vps.sub_layer_ordering_info_present_flag = r.read_bit()?;

		let start = if vps.sub_layer_ordering_info_present_flag {
			0
		} else {
			vps.max_sub_layers_minus1
		} as usize;

		for i in start..=usize::from(vps.max_sub_layers_minus1) {
			vps.max_dec_pic_buffering_minus1[i] = r.read_ue_max(15)?;
			vps.max_num_reorder_pics[i] = r.read_ue_max(vps.max_dec_pic_buffering_minus1[i])?;
			vps.max_latency_increase_plus1[i] = r.read_ue()?;

			if i > 0 {
				if vps.max_dec_pic_buffering_minus1[i] < vps.max_dec_pic_buffering_minus1[i - 1] {
					return Err(format!(
						"Invalid max_dec_pic_buffering_minus1[{}]: {}",
						i, vps.max_dec_pic_buffering_minus1[i]
					));
				}

				if vps.max_num_reorder_pics[i] < vps.max_num_reorder_pics[i - 1] {
					return Err(format!(
						"Invalid max_num_reorder_pics[{}]: {}",
						i, vps.max_num_reorder_pics[i]
					));
				}
			}
		}

		// vps_sub_layer_ordering_info_present_flag equal to 0 specifies that
		// the values of vps_max_dec_pic_buffering_minus1[
		// vps_max_sub_layers_minus1 ], vps_max_num_reorder_pics[ vps_max_sub_
		// layers_minus1 ] and vps_max_latency_increase_plus1[
		// vps_max_sub_layers_minus1 ] apply to all sub-layers
		if !vps.sub_layer_ordering_info_present_flag {
			let max_num_sublayers = usize::from(vps.max_sub_layers_minus1);
			for i in 0..max_num_sublayers {
				vps.max_dec_pic_buffering_minus1[i] = vps.max_dec_pic_buffering_minus1[max_num_sublayers];

				vps.max_num_reorder_pics[i] = vps.max_num_reorder_pics[max_num_sublayers];

				vps.max_latency_increase_plus1[i] = vps.max_latency_increase_plus1[max_num_sublayers];
			}
		}

		vps.max_layer_id = r.read_bits(6)?;
		if vps.max_layer_id > 62 {
			return Err(format!("Invalid max_layer_id {}", vps.max_layer_id));
		}

		vps.num_layer_sets_minus1 = r.read_ue_max(1023)?;

		for _ in 1..=vps.num_layer_sets_minus1 {
			for _ in 0..=vps.max_layer_id {
				// Skip layer_id_included_flag[i][j] for now.
				r.skip_bits(1)?;
			}
		}

		vps.timing_info_present_flag = r.read_bit()?;

		if vps.timing_info_present_flag {
			vps.num_units_in_tick = r.read_bits::<u32>(31)? << 1;
			vps.num_units_in_tick |= r.read_bits::<u32>(1)?;

			vps.time_scale = r.read_bits::<u32>(31)? << 1;
			vps.time_scale |= r.read_bits::<u32>(1)?;

			vps.poc_proportional_to_timing_flag = r.read_bit()?;
			if vps.poc_proportional_to_timing_flag {
				vps.num_ticks_poc_diff_one_minus1 = r.read_ue()?;
			}

			vps.num_hrd_parameters = r.read_ue()?;

			for i in 0..vps.num_hrd_parameters as usize {
				vps.hrd_layer_set_idx.push(r.read_ue()?);
				if i > 0 {
					vps.cprms_present_flag.push(r.read_bit()?);
				}

				let mut hrd = HrdParams::default();
				Self::parse_hrd_parameters(vps.cprms_present_flag[i], vps.max_sub_layers_minus1, &mut hrd, &mut r)?;

				vps.hrd_parameters.push(hrd);
			}
		}

		vps.extension_flag = r.read_bit()?;

		if self.active_vpses.keys().len() >= MAX_VPS_COUNT {
			return Err("Broken data: Number of active VPSs > MAX_VPS_COUNT".into());
		}

		let key = vps.video_parameter_set_id;
		let vps = Rc::new(vps);
		self.active_vpses.remove(&key);
		Ok(self.active_vpses.entry(key).or_insert(vps))
	}

	fn parse_profile_tier_level(
		ptl: &mut ProfileTierLevel,
		r: &mut BitReader,
		profile_present_flag: bool,
		sps_max_sub_layers_minus_1: u8,
	) -> Result<(), String> {
		if profile_present_flag {
			ptl.general_profile_space = r.read_bits(2)?;
			ptl.general_tier_flag = r.read_bit()?;
			ptl.general_profile_idc = r.read_bits(5)?;

			for i in 0..32 {
				ptl.general_profile_compatibility_flag[i] = r.read_bit()?;
			}

			ptl.general_progressive_source_flag = r.read_bit()?;
			ptl.general_interlaced_source_flag = r.read_bit()?;
			ptl.general_non_packed_constraint_flag = r.read_bit()?;
			ptl.general_frame_only_constraint_flag = r.read_bit()?;

			if ptl.general_profile_idc == 4
				|| ptl.general_profile_compatibility_flag[4]
				|| ptl.general_profile_idc == 5
				|| ptl.general_profile_compatibility_flag[5]
				|| ptl.general_profile_idc == 6
				|| ptl.general_profile_compatibility_flag[6]
				|| ptl.general_profile_idc == 7
				|| ptl.general_profile_compatibility_flag[7]
				|| ptl.general_profile_idc == 8
				|| ptl.general_profile_compatibility_flag[8]
				|| ptl.general_profile_idc == 9
				|| ptl.general_profile_compatibility_flag[9]
				|| ptl.general_profile_idc == 10
				|| ptl.general_profile_compatibility_flag[10]
				|| ptl.general_profile_idc == 11
				|| ptl.general_profile_compatibility_flag[11]
			{
				ptl.general_max_12bit_constraint_flag = r.read_bit()?;
				ptl.general_max_10bit_constraint_flag = r.read_bit()?;
				ptl.general_max_8bit_constraint_flag = r.read_bit()?;
				ptl.general_max_422chroma_constraint_flag = r.read_bit()?;
				ptl.general_max_420chroma_constraint_flag = r.read_bit()?;
				ptl.general_max_monochrome_constraint_flag = r.read_bit()?;
				ptl.general_intra_constraint_flag = r.read_bit()?;
				ptl.general_one_picture_only_constraint_flag = r.read_bit()?;
				ptl.general_lower_bit_rate_constraint_flag = r.read_bit()?;
				if ptl.general_profile_idc == 5
					|| ptl.general_profile_compatibility_flag[5]
					|| ptl.general_profile_idc == 9
					|| ptl.general_profile_compatibility_flag[9]
					|| ptl.general_profile_idc == 10
					|| ptl.general_profile_compatibility_flag[10]
					|| ptl.general_profile_idc == 11
					|| ptl.general_profile_compatibility_flag[11]
				{
					ptl.general_max_14bit_constraint_flag = r.read_bit()?;
					// Skip general_reserved_zero_33bits
					r.skip_bits(31)?;
					r.skip_bits(2)?;
				} else {
					// Skip general_reserved_zero_34bits
					r.skip_bits(31)?;
					r.skip_bits(3)?;
				}
			} else if ptl.general_profile_idc == 2 || ptl.general_profile_compatibility_flag[2] {
				// Skip general_reserved_zero_7bits
				r.skip_bits(7)?;
				ptl.general_one_picture_only_constraint_flag = r.read_bit()?;
				// Skip general_reserved_zero_35bits
				r.skip_bits(31)?;
				r.skip_bits(4)?;
			} else {
				r.skip_bits(31)?;
				r.skip_bits(12)?;
			}

			if ptl.general_profile_idc == 1
				|| ptl.general_profile_compatibility_flag[1]
				|| ptl.general_profile_idc == 2
				|| ptl.general_profile_compatibility_flag[2]
				|| ptl.general_profile_idc == 3
				|| ptl.general_profile_compatibility_flag[3]
				|| ptl.general_profile_idc == 4
				|| ptl.general_profile_compatibility_flag[4]
				|| ptl.general_profile_idc == 5
				|| ptl.general_profile_compatibility_flag[5]
				|| ptl.general_profile_idc == 9
				|| ptl.general_profile_compatibility_flag[9]
				|| ptl.general_profile_idc == 11
				|| ptl.general_profile_compatibility_flag[11]
			{
				ptl.general_inbld_flag = r.read_bit()?;
			} else {
				r.skip_bits(1)?;
			}
		}

		let level: u8 = r.read_bits(8)?;
		ptl.general_level_idc = Level::try_from(level)?;

		for i in 0..sps_max_sub_layers_minus_1 as usize {
			ptl.sub_layer_profile_present_flag[i] = r.read_bit()?;
			ptl.sub_layer_level_present_flag[i] = r.read_bit()?;
		}

		if sps_max_sub_layers_minus_1 > 0 {
			for _ in sps_max_sub_layers_minus_1..8 {
				r.skip_bits(2)?;
			}
		}

		for i in 0..sps_max_sub_layers_minus_1 as usize {
			if ptl.sub_layer_profile_present_flag[i] {
				ptl.sub_layer_profile_space[i] = r.read_bits(2)?;
				ptl.sub_layer_tier_flag[i] = r.read_bit()?;
				ptl.sub_layer_profile_idc[i] = r.read_bits(5)?;
				for j in 0..32 {
					ptl.sub_layer_profile_compatibility_flag[i][j] = r.read_bit()?;
				}
				ptl.sub_layer_progressive_source_flag[i] = r.read_bit()?;
				ptl.sub_layer_interlaced_source_flag[i] = r.read_bit()?;
				ptl.sub_layer_non_packed_constraint_flag[i] = r.read_bit()?;
				ptl.sub_layer_frame_only_constraint_flag[i] = r.read_bit()?;

				if ptl.sub_layer_profile_idc[i] == 4
					|| ptl.sub_layer_profile_compatibility_flag[i][4]
					|| ptl.sub_layer_profile_idc[i] == 5
					|| ptl.sub_layer_profile_compatibility_flag[i][5]
					|| ptl.sub_layer_profile_idc[i] == 6
					|| ptl.sub_layer_profile_compatibility_flag[i][6]
					|| ptl.sub_layer_profile_idc[i] == 7
					|| ptl.sub_layer_profile_compatibility_flag[i][7]
					|| ptl.sub_layer_profile_idc[i] == 8
					|| ptl.sub_layer_profile_compatibility_flag[i][8]
					|| ptl.sub_layer_profile_idc[i] == 9
					|| ptl.sub_layer_profile_compatibility_flag[i][9]
					|| ptl.sub_layer_profile_idc[i] == 10
					|| ptl.sub_layer_profile_compatibility_flag[i][10]
					|| ptl.sub_layer_profile_idc[i] == 11
					|| ptl.sub_layer_profile_compatibility_flag[i][11]
				{
					ptl.sub_layer_max_12bit_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_max_10bit_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_max_8bit_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_max_422chroma_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_max_420chroma_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_max_monochrome_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_intra_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_one_picture_only_constraint_flag[i] = r.read_bit()?;
					ptl.sub_layer_lower_bit_rate_constraint_flag[i] = r.read_bit()?;

					if ptl.sub_layer_profile_idc[i] == 5
						|| ptl.sub_layer_profile_compatibility_flag[i][5]
						|| ptl.sub_layer_profile_idc[i] == 9
						|| ptl.sub_layer_profile_compatibility_flag[i][9]
						|| ptl.sub_layer_profile_idc[i] == 10
						|| ptl.sub_layer_profile_compatibility_flag[i][10]
						|| ptl.sub_layer_profile_idc[i] == 11
						|| ptl.sub_layer_profile_compatibility_flag[i][11]
					{
						ptl.sub_layer_max_14bit_constraint_flag[i] = r.read_bit()?;
						r.skip_bits(33)?;
					} else {
						r.skip_bits(34)?;
					}
				} else if ptl.sub_layer_profile_idc[i] == 2 || ptl.sub_layer_profile_compatibility_flag[i][2] {
					r.skip_bits(7)?;
					ptl.sub_layer_one_picture_only_constraint_flag[i] = r.read_bit()?;
					r.skip_bits(35)?;
				} else {
					r.skip_bits(43)?;
				}

				if ptl.sub_layer_profile_idc[i] == 1
					|| ptl.sub_layer_profile_compatibility_flag[i][1]
					|| ptl.sub_layer_profile_idc[i] == 2
					|| ptl.sub_layer_profile_compatibility_flag[i][2]
					|| ptl.sub_layer_profile_idc[i] == 3
					|| ptl.sub_layer_profile_compatibility_flag[i][3]
					|| ptl.sub_layer_profile_idc[i] == 4
					|| ptl.sub_layer_profile_compatibility_flag[i][4]
					|| ptl.sub_layer_profile_idc[i] == 5
					|| ptl.sub_layer_profile_compatibility_flag[i][5]
					|| ptl.sub_layer_profile_idc[i] == 9
					|| ptl.sub_layer_profile_compatibility_flag[i][9]
					|| ptl.sub_layer_profile_idc[i] == 11
					|| ptl.sub_layer_profile_compatibility_flag[i][11]
				{
					ptl.sub_layer_inbld_flag[i] = r.read_bit()?;
				} else {
					r.skip_bits(1)?;
				}
			}

			if ptl.sub_layer_level_present_flag[i] {
				let level: u8 = r.read_bits(8)?;
				ptl.sub_layer_level_idc[i] = Level::try_from(level)?;
			}
		}
		Ok(())
	}

	fn fill_default_scaling_list(sl: &mut ScalingLists, size_id: i32, matrix_id: i32) {
		if size_id == 0 {
			sl.scaling_list_4x4[matrix_id as usize] = DEFAULT_SCALING_LIST_0;
			return;
		}

		let dst = match size_id {
			1 => &mut sl.scaling_list_8x8[matrix_id as usize],
			2 => &mut sl.scaling_list_16x16[matrix_id as usize],
			3 => &mut sl.scaling_list_32x32[matrix_id as usize],
			_ => panic!("Invalid size_id {}", size_id),
		};

		let src = if matrix_id < 3 {
			&DEFAULT_SCALING_LIST_1
		} else if matrix_id <= 5 {
			&DEFAULT_SCALING_LIST_2
		} else {
			panic!("Invalid matrix_id {}", matrix_id);
		};

		*dst = *src;

		//  When `scaling_list_pred_mode_flag[ sizeId ]`[ matrixId ] is equal to
		//  0, scaling_list_pred_matrix_id_ `delta[ sizeId ]`[ matrixId ] is equal
		//  to 0 and sizeId is greater than 1, the value of
		//  scaling_list_dc_coef_minus8[ sizeId − 2 `][ matrixId ]` is inferred to
		//  be equal to 8.
		//
		// Since we are using a slightly different layout here, with two
		// different field names (i.e. 16x16, and 32x32), we must differentiate
		// between size_id == 2 or size_id == 3.
		if size_id == 2 {
			sl.scaling_list_dc_coef_minus8_16x16[matrix_id as usize] = 8;
		} else if size_id == 3 {
			sl.scaling_list_dc_coef_minus8_32x32[matrix_id as usize] = 8;
		}
	}

	fn parse_scaling_list_data(sl: &mut ScalingLists, r: &mut BitReader) -> Result<(), String> {
		// 7.4.5
		for size_id in 0..4 {
			let mut matrix_id = 0;
			while matrix_id < 6 {
				let scaling_list_pred_mode_flag = r.read_bit()?;
				// If `scaling_list_pred_matrix_id_delta[ sizeId ]`[ matrixId ] is
				// equal to 0, the scaling list is inferred from the default
				// scaling list `ScalingList[ sizeId ]`[ matrixId `][ i ]` as specified
				// in Table 7-5 and Table 7-6 for i = 0..Min( 63, ( 1 << ( 4 + (
				// sizeId << 1 ) ) ) − 1 ).
				if !scaling_list_pred_mode_flag {
					let scaling_list_pred_matrix_id_delta: u32 = r.read_ue()?;
					if scaling_list_pred_matrix_id_delta == 0 {
						Self::fill_default_scaling_list(sl, size_id, matrix_id);
					} else {
						// Equation 7-42
						let factor = if size_id == 3 { 3 } else { 1 };
						let ref_matrix_id = matrix_id as u32 - scaling_list_pred_matrix_id_delta * factor;
						if size_id == 0 {
							sl.scaling_list_4x4[matrix_id as usize] = sl.scaling_list_4x4[ref_matrix_id as usize];
						} else {
							let src = match size_id {
								1 => sl.scaling_list_8x8[ref_matrix_id as usize],
								2 => sl.scaling_list_16x16[ref_matrix_id as usize],
								3 => sl.scaling_list_32x32[ref_matrix_id as usize],
								_ => return Err(format!("Invalid size_id {}", size_id)),
							};

							let dst = match size_id {
								1 => &mut sl.scaling_list_8x8[matrix_id as usize],
								2 => &mut sl.scaling_list_16x16[matrix_id as usize],
								3 => &mut sl.scaling_list_32x32[matrix_id as usize],
								_ => return Err(format!("Invalid size_id {}", size_id)),
							};

							*dst = src;

							if size_id == 2 {
								sl.scaling_list_dc_coef_minus8_16x16[matrix_id as usize] =
									sl.scaling_list_dc_coef_minus8_16x16[ref_matrix_id as usize];
							} else if size_id == 3 {
								sl.scaling_list_dc_coef_minus8_32x32[matrix_id as usize] =
									sl.scaling_list_dc_coef_minus8_32x32[ref_matrix_id as usize];
							}
						}
					}
				} else {
					let mut next_coef = 8i32;
					let coef_num = std::cmp::min(64, 1 << (4 + (size_id << 1)));

					if size_id > 1 {
						if size_id == 2 {
							sl.scaling_list_dc_coef_minus8_16x16[matrix_id as usize] = r.read_se_bounded(-7, 247)?;
							next_coef = i32::from(sl.scaling_list_dc_coef_minus8_16x16[matrix_id as usize]) + 8;
						} else if size_id == 3 {
							sl.scaling_list_dc_coef_minus8_32x32[matrix_id as usize] = r.read_se_bounded(-7, 247)?;
							next_coef = i32::from(sl.scaling_list_dc_coef_minus8_32x32[matrix_id as usize]) + 8;
						}
					}

					for i in 0..coef_num as usize {
						let scaling_list_delta_coef: i32 = r.read_se_bounded(-128, 127)?;
						next_coef = (next_coef + scaling_list_delta_coef + 256) % 256;
						match size_id {
							0 => sl.scaling_list_4x4[matrix_id as usize][i] = next_coef as _,
							1 => sl.scaling_list_8x8[matrix_id as usize][i] = next_coef as _,
							2 => sl.scaling_list_16x16[matrix_id as usize][i] = next_coef as _,
							3 => sl.scaling_list_32x32[matrix_id as usize][i] = next_coef as _,
							_ => return Err(format!("Invalid size_id {}", size_id)),
						}
					}
				}
				let step = if size_id == 3 { 3 } else { 1 };
				matrix_id += step;
			}
		}
		Ok(())
	}

	fn parse_short_term_ref_pic_set(
		sps: &Sps,
		st: &mut ShortTermRefPicSet,
		r: &mut BitReader,
		st_rps_idx: u8,
	) -> Result<(), String> {
		if st_rps_idx != 0 {
			st.inter_ref_pic_set_prediction_flag = r.read_bit()?;
		}

		// (7-59)
		if st.inter_ref_pic_set_prediction_flag {
			if st_rps_idx == sps.num_short_term_ref_pic_sets {
				st.delta_idx_minus1 = r.read_ue_max(st_rps_idx as u32 - 1)?;
			}

			st.delta_rps_sign = r.read_bit()?;
			// The value of abs_delta_rps_minus1 shall be in the range of 0 to
			// 2^15 − 1, inclusive.
			st.abs_delta_rps_minus1 = r.read_ue_max(32767)?;

			let ref_rps_idx = st_rps_idx - (st.delta_idx_minus1 + 1);
			let delta_rps = (1 - 2 * st.delta_rps_sign as i32) * (st.abs_delta_rps_minus1 as i32 + 1);

			let ref_st = sps
				.short_term_ref_pic_set
				.get(usize::from(ref_rps_idx))
				.ok_or::<String>("Invalid ref_rps_idx".into())?;

			let mut used_by_curr_pic_flag = [false; 64];

			// 7.4.8 - defaults to 1 if not present
			let mut use_delta_flag = [true; 64];

			for j in 0..=ref_st.num_delta_pocs as usize {
				used_by_curr_pic_flag[j] = r.read_bit()?;
				if !used_by_curr_pic_flag[j] {
					use_delta_flag[j] = r.read_bit()?;
				}
			}

			// (7-61)
			let mut i = 0;
			// Ranges are [a,b[, but the real loop is [b, a], i.e.
			// [num_positive_pics - 1, 0]. Use ..= so that b is included when
			// rev() is called.
			for j in (0..=isize::from(ref_st.num_positive_pics) - 1)
				.rev()
				.take_while(|j| *j >= 0)
				.map(|j| j as usize)
			{
				let d_poc = ref_st.delta_poc_s1[j] + delta_rps;
				if d_poc < 0 && use_delta_flag[usize::from(ref_st.num_negative_pics) + j] {
					st.delta_poc_s0[i] = d_poc;
					st.used_by_curr_pic_s0[i] = used_by_curr_pic_flag[usize::from(ref_st.num_negative_pics) + j];

					i += 1;
				}
			}

			if delta_rps < 0 && use_delta_flag[ref_st.num_delta_pocs as usize] {
				st.delta_poc_s0[i] = delta_rps;
				st.used_by_curr_pic_s0[i] = used_by_curr_pic_flag[ref_st.num_delta_pocs as usize];

				i += 1;
			}

			// Let's *not* change the original algorithm in any way.
			for j in 0..ref_st.num_negative_pics as usize {
				let d_poc = ref_st.delta_poc_s0[j] + delta_rps;
				if d_poc < 0 && use_delta_flag[j] {
					st.delta_poc_s0[i] = d_poc;
					st.used_by_curr_pic_s0[i] = used_by_curr_pic_flag[j];

					i += 1;
				}
			}

			st.num_negative_pics = i as u8;

			// (7-62)
			let mut i = 0;
			// Ranges are [a,b[, but the real loop is [b, a], i.e.
			// [num_negative_pics - 1, 0]. Use ..= so that b is included when
			// rev() is called.
			for j in (0..=isize::from(ref_st.num_negative_pics) - 1)
				.rev()
				.take_while(|j| *j >= 0)
				.map(|j| j as usize)
			{
				let d_poc = ref_st.delta_poc_s0[j] + delta_rps;
				if d_poc > 0 && use_delta_flag[j] {
					st.delta_poc_s1[i] = d_poc;
					st.used_by_curr_pic_s1[i] = used_by_curr_pic_flag[j];

					i += 1;
				}
			}

			if delta_rps > 0 && use_delta_flag[ref_st.num_delta_pocs as usize] {
				st.delta_poc_s1[i] = delta_rps;
				st.used_by_curr_pic_s1[i] = used_by_curr_pic_flag[ref_st.num_delta_pocs as usize];

				i += 1;
			}

			for j in 0..usize::from(ref_st.num_positive_pics) {
				let d_poc = ref_st.delta_poc_s1[j] + delta_rps;
				if d_poc > 0 && use_delta_flag[ref_st.num_negative_pics as usize + j] {
					st.delta_poc_s1[i] = d_poc;
					st.used_by_curr_pic_s1[i] = used_by_curr_pic_flag[ref_st.num_negative_pics as usize + j];

					i += 1;
				}
			}

			st.num_positive_pics = i as u8;
		} else {
			st.num_negative_pics = r.read_ue_max(u32::from(
				sps.max_dec_pic_buffering_minus1[usize::from(sps.max_sub_layers_minus1)],
			))?;

			st.num_positive_pics = r.read_ue_max(u32::from(
				sps.max_dec_pic_buffering_minus1[usize::from(sps.max_sub_layers_minus1)] - st.num_negative_pics,
			))?;

			for i in 0..usize::from(st.num_negative_pics) {
				let delta_poc_s0_minus1: u32 = r.read_ue_max(32767)?;

				if i == 0 {
					st.delta_poc_s0[i] = -(delta_poc_s0_minus1 as i32 + 1);
				} else {
					st.delta_poc_s0[i] = st.delta_poc_s0[i - 1] - (delta_poc_s0_minus1 as i32 + 1);
				}

				st.used_by_curr_pic_s0[i] = r.read_bit()?;
			}

			for i in 0..usize::from(st.num_positive_pics) {
				let delta_poc_s1_minus1: u32 = r.read_ue_max(32767)?;

				if i == 0 {
					st.delta_poc_s1[i] = delta_poc_s1_minus1 as i32 + 1;
				} else {
					st.delta_poc_s1[i] = st.delta_poc_s1[i - 1] + (delta_poc_s1_minus1 as i32 + 1);
				}

				st.used_by_curr_pic_s1[i] = r.read_bit()?;
			}
		}

		st.num_delta_pocs = u32::from(st.num_negative_pics + st.num_positive_pics);

		Ok(())
	}

	fn parse_sublayer_hrd_parameters(
		h: &mut SublayerHrdParameters,
		cpb_cnt: u32,
		sub_pic_hrd_params_present_flag: bool,
		r: &mut BitReader,
	) -> Result<(), String> {
		for i in 0..cpb_cnt as usize {
			h.bit_rate_value_minus1[i] = r.read_ue_max((2u64.pow(32) - 2) as u32)?;
			h.cpb_size_value_minus1[i] = r.read_ue_max((2u64.pow(32) - 2) as u32)?;
			if sub_pic_hrd_params_present_flag {
				h.cpb_size_du_value_minus1[i] = r.read_ue_max((2u64.pow(32) - 2) as u32)?;
				h.bit_rate_du_value_minus1[i] = r.read_ue_max((2u64.pow(32) - 2) as u32)?;
			}

			h.cbr_flag[i] = r.read_bit()?;
		}

		Ok(())
	}

	fn parse_hrd_parameters(
		common_inf_present_flag: bool,
		max_num_sublayers_minus1: u8,
		hrd: &mut HrdParams,
		r: &mut BitReader,
	) -> Result<(), String> {
		if common_inf_present_flag {
			hrd.nal_hrd_parameters_present_flag = r.read_bit()?;
			hrd.vcl_hrd_parameters_present_flag = r.read_bit()?;
			if hrd.nal_hrd_parameters_present_flag || hrd.vcl_hrd_parameters_present_flag {
				hrd.sub_pic_hrd_params_present_flag = r.read_bit()?;
				if hrd.sub_pic_hrd_params_present_flag {
					hrd.tick_divisor_minus2 = r.read_bits(8)?;
					hrd.du_cpb_removal_delay_increment_length_minus1 = r.read_bits(5)?;
					hrd.sub_pic_cpb_params_in_pic_timing_sei_flag = r.read_bit()?;
					hrd.dpb_output_delay_du_length_minus1 = r.read_bits(5)?;
				}
				hrd.bit_rate_scale = r.read_bits(4)?;
				hrd.cpb_size_scale = r.read_bits(4)?;
				if hrd.sub_pic_hrd_params_present_flag {
					hrd.cpb_size_du_scale = r.read_bits(4)?;
				}
				hrd.initial_cpb_removal_delay_length_minus1 = r.read_bits(5)?;
				hrd.au_cpb_removal_delay_length_minus1 = r.read_bits(5)?;
				hrd.dpb_output_delay_length_minus1 = r.read_bits(5)?;
			}
		}

		for i in 0..=max_num_sublayers_minus1 as usize {
			hrd.fixed_pic_rate_general_flag[i] = r.read_bit()?;
			if !hrd.fixed_pic_rate_general_flag[i] {
				hrd.fixed_pic_rate_within_cvs_flag[i] = r.read_bit()?;
			}
			if hrd.fixed_pic_rate_within_cvs_flag[i] {
				hrd.elemental_duration_in_tc_minus1[i] = r.read_ue_max(2047)?;
			} else {
				hrd.low_delay_hrd_flag[i] = r.read_bit()?;
			}

			if !hrd.low_delay_hrd_flag[i] {
				hrd.cpb_cnt_minus1[i] = r.read_ue_max(31)?;
			}

			if hrd.nal_hrd_parameters_present_flag {
				Self::parse_sublayer_hrd_parameters(
					&mut hrd.nal_hrd[i],
					hrd.cpb_cnt_minus1[i] + 1,
					hrd.sub_pic_hrd_params_present_flag,
					r,
				)?;
			}

			if hrd.vcl_hrd_parameters_present_flag {
				Self::parse_sublayer_hrd_parameters(
					&mut hrd.vcl_hrd[i],
					hrd.cpb_cnt_minus1[i] + 1,
					hrd.sub_pic_hrd_params_present_flag,
					r,
				)?;
			}
		}

		Ok(())
	}

	fn parse_vui_parameters(sps: &mut Sps, r: &mut BitReader) -> Result<(), String> {
		let vui = &mut sps.vui_parameters;

		vui.aspect_ratio_info_present_flag = r.read_bit()?;
		if vui.aspect_ratio_info_present_flag {
			vui.aspect_ratio_idc = r.read_bits(8)?;
			const EXTENDED_SAR: u32 = 255;
			if vui.aspect_ratio_idc == EXTENDED_SAR {
				vui.sar_width = r.read_bits(16)?;
				vui.sar_height = r.read_bits(16)?;
			}
		}

		vui.overscan_info_present_flag = r.read_bit()?;
		if vui.overscan_info_present_flag {
			vui.overscan_appropriate_flag = r.read_bit()?;
		}

		vui.video_signal_type_present_flag = r.read_bit()?;
		if vui.video_signal_type_present_flag {
			vui.video_format = r.read_bits(3)?;
			vui.video_full_range_flag = r.read_bit()?;
			vui.colour_description_present_flag = r.read_bit()?;
			if vui.colour_description_present_flag {
				vui.colour_primaries = r.read_bits(8)?;
				vui.transfer_characteristics = r.read_bits(8)?;
				vui.matrix_coeffs = r.read_bits(8)?;
			}
		}

		vui.chroma_loc_info_present_flag = r.read_bit()?;
		if vui.chroma_loc_info_present_flag {
			vui.chroma_sample_loc_type_top_field = r.read_ue_max(5)?;
			vui.chroma_sample_loc_type_bottom_field = r.read_ue_max(5)?;
		}

		vui.neutral_chroma_indication_flag = r.read_bit()?;
		vui.field_seq_flag = r.read_bit()?;
		vui.frame_field_info_present_flag = r.read_bit()?;
		vui.default_display_window_flag = r.read_bit()?;

		if vui.default_display_window_flag {
			vui.def_disp_win_left_offset = r.read_ue()?;
			vui.def_disp_win_right_offset = r.read_ue()?;
			vui.def_disp_win_top_offset = r.read_ue()?;
			vui.def_disp_win_bottom_offset = r.read_ue()?;
		}

		vui.timing_info_present_flag = r.read_bit()?;
		if vui.timing_info_present_flag {
			vui.num_units_in_tick = r.read_bits::<u32>(31)? << 1;
			vui.num_units_in_tick |= r.read_bits::<u32>(1)?;

			if vui.num_units_in_tick == 0 {
				tracing::warn!("Incompliant value for num_units_in_tick {}", vui.num_units_in_tick);
			}

			vui.time_scale = r.read_bits::<u32>(31)? << 1;
			vui.time_scale |= r.read_bits::<u32>(1)?;

			if vui.time_scale == 0 {
				tracing::warn!("Incompliant value for time_scale {}", vui.time_scale);
			}

			vui.poc_proportional_to_timing_flag = r.read_bit()?;
			if vui.poc_proportional_to_timing_flag {
				vui.num_ticks_poc_diff_one_minus1 = r.read_ue_max((2u64.pow(32) - 2) as u32)?;
			}

			vui.hrd_parameters_present_flag = r.read_bit()?;
			if vui.hrd_parameters_present_flag {
				let sps_max_sub_layers_minus1 = sps.max_sub_layers_minus1;
				Self::parse_hrd_parameters(true, sps_max_sub_layers_minus1, &mut vui.hrd, r)?;
			}
		}

		vui.bitstream_restriction_flag = r.read_bit()?;
		if vui.bitstream_restriction_flag {
			vui.tiles_fixed_structure_flag = r.read_bit()?;
			vui.motion_vectors_over_pic_boundaries_flag = r.read_bit()?;
			vui.restricted_ref_pic_lists_flag = r.read_bit()?;

			vui.min_spatial_segmentation_idc = r.read_ue_max(4095)?;
			vui.max_bytes_per_pic_denom = r.read_ue()?;
			vui.max_bits_per_min_cu_denom = r.read_ue()?;
			vui.log2_max_mv_length_horizontal = r.read_ue_max(16)?;
			vui.log2_max_mv_length_vertical = r.read_ue_max(15)?;
		}

		Ok(())
	}

	fn parse_sps_scc_extension(sps: &mut Sps, r: &mut BitReader) -> Result<(), String> {
		let scc = &mut sps.scc_extension;

		scc.curr_pic_ref_enabled_flag = r.read_bit()?;
		scc.palette_mode_enabled_flag = r.read_bit()?;
		if scc.palette_mode_enabled_flag {
			scc.palette_max_size = r.read_ue_max(64)?;
			scc.delta_palette_max_predictor_size = r.read_ue_max(128 - u32::from(scc.palette_max_size))?;
			scc.palette_predictor_initializers_present_flag = r.read_bit()?;
			if scc.palette_predictor_initializers_present_flag {
				let max = u32::from(scc.palette_max_size + scc.delta_palette_max_predictor_size - 1);
				scc.num_palette_predictor_initializer_minus1 = r.read_ue_max(max)?;

				let num_comps = if sps.chroma_format_idc == 0 { 1 } else { 3 };
				for comp in 0..num_comps {
					for i in 0..=usize::from(scc.num_palette_predictor_initializer_minus1) {
						let num_bits = if comp == 0 {
							sps.bit_depth_luma_minus8 + 8
						} else {
							sps.bit_depth_chroma_minus8 + 8
						};
						scc.palette_predictor_initializer[comp][i] = r.read_bits(usize::from(num_bits))?;
					}
				}
			}
		}

		scc.motion_vector_resolution_control_idc = r.read_bits(2)?;
		scc.intra_boundary_filtering_disabled_flag = r.read_bit()?;

		Ok(())
	}

	fn parse_sps_range_extension(sps: &mut Sps, r: &mut BitReader) -> Result<(), String> {
		let ext = &mut sps.range_extension;

		ext.transform_skip_rotation_enabled_flag = r.read_bit()?;
		ext.transform_skip_context_enabled_flag = r.read_bit()?;
		ext.implicit_rdpcm_enabled_flag = r.read_bit()?;
		ext.explicit_rdpcm_enabled_flag = r.read_bit()?;
		ext.extended_precision_processing_flag = r.read_bit()?;
		ext.intra_smoothing_disabled_flag = r.read_bit()?;
		ext.high_precision_offsets_enabled_flag = r.read_bit()?;
		ext.persistent_rice_adaptation_enabled_flag = r.read_bit()?;
		ext.cabac_bypass_alignment_enabled_flag = r.read_bit()?;

		Ok(())
	}

	/// Parse a SPS NALU.
	pub fn parse_sps(&mut self, nalu: &Nalu) -> Result<&Sps, String> {
		if !matches!(nalu.header.type_, NaluType::SpsNut) {
			return Err(format!(
				"Invalid NALU type, expected {:?}, got {:?}",
				NaluType::SpsNut,
				nalu.header.type_
			));
		}

		let data = nalu.as_ref();
		let header = &nalu.header;
		let hdr_len = header.len();
		// Skip the header
		let mut r = BitReader::new(&data[hdr_len..], true);

		let video_parameter_set_id = r.read_bits(4)?;

		// A non-existing VPS means the SPS is not using any VPS.
		let vps = self.get_vps(video_parameter_set_id).cloned();

		let mut sps = Sps {
			video_parameter_set_id,
			max_sub_layers_minus1: r.read_bits(3)?,
			temporal_id_nesting_flag: r.read_bit()?,
			vps,
			..Default::default()
		};

		Self::parse_profile_tier_level(&mut sps.profile_tier_level, &mut r, true, sps.max_sub_layers_minus1)?;

		sps.seq_parameter_set_id = r.read_ue_max(MAX_SPS_COUNT as u32 - 1)?;
		sps.chroma_format_idc = r.read_ue_max(3)?;

		if sps.chroma_format_idc == 3 {
			sps.separate_colour_plane_flag = r.read_bit()?;
		}

		sps.chroma_array_type = if sps.separate_colour_plane_flag {
			0
		} else {
			sps.chroma_format_idc
		};

		sps.pic_width_in_luma_samples = r.read_ue_bounded(1, 16888)?;
		sps.pic_height_in_luma_samples = r.read_ue_bounded(1, 16888)?;

		sps.conformance_window_flag = r.read_bit()?;
		if sps.conformance_window_flag {
			sps.conf_win_left_offset = r.read_ue()?;
			sps.conf_win_right_offset = r.read_ue()?;
			sps.conf_win_top_offset = r.read_ue()?;
			sps.conf_win_bottom_offset = r.read_ue()?;
		}

		sps.bit_depth_luma_minus8 = r.read_ue_max(6)?;
		sps.bit_depth_chroma_minus8 = r.read_ue_max(6)?;
		sps.log2_max_pic_order_cnt_lsb_minus4 = r.read_ue_max(12)?;
		sps.sub_layer_ordering_info_present_flag = r.read_bit()?;

		{
			let i = if sps.sub_layer_ordering_info_present_flag {
				0
			} else {
				sps.max_sub_layers_minus1
			};

			for j in i..=sps.max_sub_layers_minus1 {
				sps.max_dec_pic_buffering_minus1[j as usize] = r.read_ue_max(16)?;
				sps.max_num_reorder_pics[j as usize] =
					r.read_ue_max(sps.max_dec_pic_buffering_minus1[j as usize] as _)?;
				sps.max_latency_increase_plus1[j as usize] = r.read_ue_max(u32::MAX - 1)?;
			}
		}

		sps.log2_min_luma_coding_block_size_minus3 = r.read_ue_max(3)?;
		sps.log2_diff_max_min_luma_coding_block_size = r.read_ue_max(6)?;
		sps.log2_min_luma_transform_block_size_minus2 = r.read_ue_max(3)?;
		sps.log2_diff_max_min_luma_transform_block_size = r.read_ue_max(3)?;

		// (7-10)
		sps.min_cb_log2_size_y = u32::from(sps.log2_min_luma_coding_block_size_minus3 + 3);
		// (7-11)
		sps.ctb_log2_size_y = sps.min_cb_log2_size_y + u32::from(sps.log2_diff_max_min_luma_coding_block_size);
		// (7-12)
		sps.ctb_size_y = 1 << sps.ctb_log2_size_y;
		// (7-17)
		sps.pic_height_in_ctbs_y = (sps.pic_height_in_luma_samples as f64 / sps.ctb_size_y as f64).ceil() as u32;
		// (7-15)
		sps.pic_width_in_ctbs_y = (sps.pic_width_in_luma_samples as f64 / sps.ctb_size_y as f64).ceil() as u32;

		sps.max_tb_log2_size_y = u32::from(
			sps.log2_min_luma_transform_block_size_minus2 + 2 + sps.log2_diff_max_min_luma_transform_block_size,
		);

		sps.pic_size_in_samples_y =
			u32::from(sps.pic_width_in_luma_samples) * u32::from(sps.pic_height_in_luma_samples);

		if sps.max_tb_log2_size_y > std::cmp::min(sps.ctb_log2_size_y, 5) {
			return Err(format!("Invalid value for MaxTbLog2SizeY: {}", sps.max_tb_log2_size_y));
		}

		sps.pic_size_in_ctbs_y = sps.pic_width_in_ctbs_y * sps.pic_height_in_ctbs_y;

		sps.max_transform_hierarchy_depth_inter = r.read_ue_max(4)?;
		sps.max_transform_hierarchy_depth_intra = r.read_ue_max(4)?;

		sps.scaling_list_enabled_flag = r.read_bit()?;
		if sps.scaling_list_enabled_flag {
			sps.scaling_list_data_present_flag = r.read_bit()?;
			if sps.scaling_list_data_present_flag {
				Self::parse_scaling_list_data(&mut sps.scaling_list, &mut r)?;
			}
		}

		sps.amp_enabled_flag = r.read_bit()?;
		sps.sample_adaptive_offset_enabled_flag = r.read_bit()?;

		sps.pcm_enabled_flag = r.read_bit()?;
		if sps.pcm_enabled_flag {
			sps.pcm_sample_bit_depth_luma_minus1 = r.read_bits(4)?;
			sps.pcm_sample_bit_depth_chroma_minus1 = r.read_bits(4)?;
			sps.log2_min_pcm_luma_coding_block_size_minus3 = r.read_ue_max(2)?;
			sps.log2_diff_max_min_pcm_luma_coding_block_size = r.read_ue_max(2)?;
			sps.pcm_loop_filter_disabled_flag = r.read_bit()?;
		}

		sps.num_short_term_ref_pic_sets = r.read_ue_max(64)?;

		for i in 0..sps.num_short_term_ref_pic_sets {
			let mut st = ShortTermRefPicSet::default();
			Self::parse_short_term_ref_pic_set(&sps, &mut st, &mut r, i)?;
			sps.short_term_ref_pic_set.push(st);
		}

		sps.long_term_ref_pics_present_flag = r.read_bit()?;
		if sps.long_term_ref_pics_present_flag {
			sps.num_long_term_ref_pics_sps = r.read_ue_max(32)?;
			for i in 0..usize::from(sps.num_long_term_ref_pics_sps) {
				sps.lt_ref_pic_poc_lsb_sps[i] = r.read_bits(usize::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4)?;
				sps.used_by_curr_pic_lt_sps_flag[i] = r.read_bit()?;
			}
		}

		sps.temporal_mvp_enabled_flag = r.read_bit()?;
		sps.strong_intra_smoothing_enabled_flag = r.read_bit()?;

		sps.vui_parameters_present_flag = r.read_bit()?;
		if sps.vui_parameters_present_flag {
			Self::parse_vui_parameters(&mut sps, &mut r)?;
		}

		sps.extension_present_flag = r.read_bit()?;
		if sps.extension_present_flag {
			sps.range_extension_flag = r.read_bit()?;
			if sps.range_extension_flag {
				Self::parse_sps_range_extension(&mut sps, &mut r)?;
			}

			let multilayer_extension_flag = r.read_bit()?;
			if multilayer_extension_flag {
				return Err("Multilayer extension not supported.".into());
			}

			let three_d_extension_flag = r.read_bit()?;
			if three_d_extension_flag {
				return Err("3D extension not supported.".into());
			}

			sps.scc_extension_flag = r.read_bit()?;
			if sps.scc_extension_flag {
				Self::parse_sps_scc_extension(&mut sps, &mut r)?;
			}
		}

		let shift = if sps.range_extension.high_precision_offsets_enabled_flag {
			sps.bit_depth_luma_minus8 + 7
		} else {
			7
		};

		sps.wp_offset_half_range_y = 1 << shift;

		let shift = if sps.range_extension.high_precision_offsets_enabled_flag {
			sps.bit_depth_chroma_minus8 + 7
		} else {
			7
		};

		sps.wp_offset_half_range_c = 1 << shift;

		tracing::debug!(
			"Parsed SPS({}), resolution: ({}, {}): NAL size was {}",
			sps.seq_parameter_set_id,
			sps.width(),
			sps.height(),
			nalu.size
		);

		if self.active_spses.keys().len() >= MAX_SPS_COUNT {
			return Err("Broken data: Number of active SPSs > MAX_SPS_COUNT".into());
		}

		let key = sps.seq_parameter_set_id;
		let sps = Rc::new(sps);
		self.active_spses.remove(&key);
		Ok(self.active_spses.entry(key).or_insert(sps))
	}

	fn parse_pps_scc_extension(pps: &mut Pps, sps: &Sps, r: &mut BitReader) -> Result<(), String> {
		let scc = &mut pps.scc_extension;
		scc.curr_pic_ref_enabled_flag = r.read_bit()?;
		scc.residual_adaptive_colour_transform_enabled_flag = r.read_bit()?;
		if scc.residual_adaptive_colour_transform_enabled_flag {
			scc.slice_act_qp_offsets_present_flag = r.read_bit()?;
			scc.act_y_qp_offset_plus5 = r.read_se_bounded(-7, 17)?;
			scc.act_cb_qp_offset_plus5 = r.read_se_bounded(-7, 17)?;
			scc.act_cr_qp_offset_plus3 = r.read_se_bounded(-9, 15)?;
		}

		scc.palette_predictor_initializers_present_flag = r.read_bit()?;
		if scc.palette_predictor_initializers_present_flag {
			let max = sps.scc_extension.palette_max_size + sps.scc_extension.delta_palette_max_predictor_size;
			scc.num_palette_predictor_initializers = r.read_ue_max(max.into())?;
			if scc.num_palette_predictor_initializers > 0 {
				scc.monochrome_palette_flag = r.read_bit()?;
				scc.luma_bit_depth_entry_minus8 =
					r.read_ue_bounded(sps.bit_depth_luma_minus8.into(), sps.bit_depth_luma_minus8.into())?;
				if !scc.monochrome_palette_flag {
					scc.chroma_bit_depth_entry_minus8 =
						r.read_ue_bounded(sps.bit_depth_chroma_minus8.into(), sps.bit_depth_chroma_minus8.into())?;
				}

				let num_comps = if scc.monochrome_palette_flag { 1 } else { 3 };
				for comp in 0..num_comps {
					let num_bits = if comp == 0 {
						scc.luma_bit_depth_entry_minus8 + 8
					} else {
						scc.chroma_bit_depth_entry_minus8 + 8
					};
					for i in 0..usize::from(scc.num_palette_predictor_initializers) {
						scc.palette_predictor_initializer[comp][i] = r.read_bits(num_bits.into())?;
					}
				}
			}
		}
		Ok(())
	}

	fn parse_pps_range_extension(pps: &mut Pps, sps: &Sps, r: &mut BitReader) -> Result<(), String> {
		let rext = &mut pps.range_extension;

		if pps.transform_skip_enabled_flag {
			rext.log2_max_transform_skip_block_size_minus2 = r.read_ue_max(sps.max_tb_log2_size_y - 2)?;
		}

		rext.cross_component_prediction_enabled_flag = r.read_bit()?;
		rext.chroma_qp_offset_list_enabled_flag = r.read_bit()?;
		if rext.chroma_qp_offset_list_enabled_flag {
			rext.diff_cu_chroma_qp_offset_depth = r.read_ue()?;
			rext.chroma_qp_offset_list_len_minus1 = r.read_ue_max(5)?;
			for i in 0..=rext.chroma_qp_offset_list_len_minus1 as usize {
				rext.cb_qp_offset_list[i] = r.read_se_bounded(-12, 12)?;
				rext.cr_qp_offset_list[i] = r.read_se_bounded(-12, 12)?;
			}
		}

		let bit_depth_y = sps.bit_depth_luma_minus8 + 8;
		let max = u32::from(std::cmp::max(0, bit_depth_y - 10));

		rext.log2_sao_offset_scale_luma = r.read_ue_max(max)?;
		rext.log2_sao_offset_scale_chroma = r.read_ue_max(max)?;

		Ok(())
	}

	/// Parse a PPS NALU.
	pub fn parse_pps(&mut self, nalu: &Nalu) -> Result<&Pps, String> {
		if !matches!(nalu.header.type_, NaluType::PpsNut) {
			return Err(format!(
				"Invalid NALU type, expected {:?}, got {:?}",
				NaluType::PpsNut,
				nalu.header.type_
			));
		}

		let data = nalu.as_ref();
		let header = &nalu.header;
		let hdr_len = header.len();
		// Skip the header
		let mut r = BitReader::new(&data[hdr_len..], true);

		let pic_parameter_set_id = r.read_ue_max(MAX_PPS_COUNT as u32 - 1)?;
		let seq_parameter_set_id = r.read_ue_max(MAX_SPS_COUNT as u32 - 1)?;

		let sps = self.get_sps(seq_parameter_set_id).ok_or::<String>(format!(
			"Could not get SPS for seq_parameter_set_id {}",
			seq_parameter_set_id
		))?;

		let mut pps = Pps {
			pic_parameter_set_id,
			seq_parameter_set_id,
			dependent_slice_segments_enabled_flag: Default::default(),
			output_flag_present_flag: Default::default(),
			num_extra_slice_header_bits: Default::default(),
			sign_data_hiding_enabled_flag: Default::default(),
			cabac_init_present_flag: Default::default(),
			num_ref_idx_l0_default_active_minus1: Default::default(),
			num_ref_idx_l1_default_active_minus1: Default::default(),
			init_qp_minus26: Default::default(),
			constrained_intra_pred_flag: Default::default(),
			transform_skip_enabled_flag: Default::default(),
			cu_qp_delta_enabled_flag: Default::default(),
			diff_cu_qp_delta_depth: Default::default(),
			cb_qp_offset: Default::default(),
			cr_qp_offset: Default::default(),
			slice_chroma_qp_offsets_present_flag: Default::default(),
			weighted_pred_flag: Default::default(),
			weighted_bipred_flag: Default::default(),
			transquant_bypass_enabled_flag: Default::default(),
			tiles_enabled_flag: Default::default(),
			entropy_coding_sync_enabled_flag: Default::default(),
			num_tile_columns_minus1: Default::default(),
			num_tile_rows_minus1: Default::default(),
			uniform_spacing_flag: true,
			column_width_minus1: Default::default(),
			row_height_minus1: Default::default(),
			loop_filter_across_tiles_enabled_flag: true,
			loop_filter_across_slices_enabled_flag: Default::default(),
			deblocking_filter_control_present_flag: Default::default(),
			deblocking_filter_override_enabled_flag: Default::default(),
			deblocking_filter_disabled_flag: Default::default(),
			beta_offset_div2: Default::default(),
			tc_offset_div2: Default::default(),
			scaling_list_data_present_flag: Default::default(),
			scaling_list: Default::default(),
			lists_modification_present_flag: Default::default(),
			log2_parallel_merge_level_minus2: Default::default(),
			slice_segment_header_extension_present_flag: Default::default(),
			extension_present_flag: Default::default(),
			range_extension_flag: Default::default(),
			range_extension: Default::default(),
			qp_bd_offset_y: Default::default(),
			scc_extension: Default::default(),
			scc_extension_flag: Default::default(),
			sps: Rc::clone(sps),
		};

		pps.dependent_slice_segments_enabled_flag = r.read_bit()?;
		pps.output_flag_present_flag = r.read_bit()?;
		pps.num_extra_slice_header_bits = r.read_bits(3)?;
		pps.sign_data_hiding_enabled_flag = r.read_bit()?;
		pps.cabac_init_present_flag = r.read_bit()?;

		// 7.4.7.1
		pps.num_ref_idx_l0_default_active_minus1 = r.read_ue_max(14)?;
		pps.num_ref_idx_l1_default_active_minus1 = r.read_ue_max(14)?;

		// (7-5)
		let qp_bd_offset_y = 6 * i32::from(sps.bit_depth_luma_minus8);

		pps.init_qp_minus26 = r.read_se_bounded(-(26 + qp_bd_offset_y), 25)?;
		pps.qp_bd_offset_y = qp_bd_offset_y as u32;
		pps.constrained_intra_pred_flag = r.read_bit()?;
		pps.transform_skip_enabled_flag = r.read_bit()?;
		pps.cu_qp_delta_enabled_flag = r.read_bit()?;

		if pps.cu_qp_delta_enabled_flag {
			pps.diff_cu_qp_delta_depth = r.read_ue_max(u32::from(sps.log2_diff_max_min_luma_coding_block_size))?;
		}

		pps.cb_qp_offset = r.read_se_bounded(-12, 12)?;
		pps.cr_qp_offset = r.read_se_bounded(-12, 12)?;

		pps.slice_chroma_qp_offsets_present_flag = r.read_bit()?;
		pps.weighted_pred_flag = r.read_bit()?;
		pps.weighted_bipred_flag = r.read_bit()?;
		pps.transquant_bypass_enabled_flag = r.read_bit()?;
		pps.tiles_enabled_flag = r.read_bit()?;
		pps.entropy_coding_sync_enabled_flag = r.read_bit()?;

		// A mix of the rbsp data and the algorithm in 6.5.1
		if pps.tiles_enabled_flag {
			pps.num_tile_columns_minus1 = r.read_ue_max(sps.pic_width_in_ctbs_y - 1)?;
			pps.num_tile_rows_minus1 = r.read_ue_max(sps.pic_height_in_ctbs_y - 1)?;
			if usize::from(pps.num_tile_columns_minus1) >= MAX_TILE_COLUMNS
				|| usize::from(pps.num_tile_rows_minus1) >= MAX_TILE_ROWS
			{
				return Err(format!(
					"{}x{} tiles, more than any level allows",
					u32::from(pps.num_tile_columns_minus1) + 1,
					u32::from(pps.num_tile_rows_minus1) + 1
				));
			}
			pps.uniform_spacing_flag = r.read_bit()?;
			if !pps.uniform_spacing_flag {
				pps.column_width_minus1[usize::from(pps.num_tile_columns_minus1)] = sps.pic_width_in_ctbs_y - 1;

				for i in 0..usize::from(pps.num_tile_columns_minus1) {
					pps.column_width_minus1[i] =
						r.read_ue_max(pps.column_width_minus1[usize::from(pps.num_tile_columns_minus1)] - 1)?;
					pps.column_width_minus1[usize::from(pps.num_tile_columns_minus1)] -= pps.column_width_minus1[i] + 1;
				}

				pps.row_height_minus1[usize::from(pps.num_tile_rows_minus1)] = sps.pic_height_in_ctbs_y - 1;

				for i in 0..usize::from(pps.num_tile_rows_minus1) {
					pps.row_height_minus1[i] =
						r.read_ue_max(pps.row_height_minus1[usize::from(pps.num_tile_rows_minus1)] - 1)?;
					pps.row_height_minus1[usize::from(pps.num_tile_rows_minus1)] -= pps.row_height_minus1[i] + 1;
				}
			} else {
				let nrows = u32::from(pps.num_tile_rows_minus1) + 1;
				let ncols = u32::from(pps.num_tile_columns_minus1) + 1;

				for j in 0..ncols {
					pps.column_width_minus1[j as usize] =
						((j + 1) * sps.pic_width_in_ctbs_y) / ncols - j * sps.pic_width_in_ctbs_y / ncols - 1;
				}

				for j in 0..nrows {
					pps.row_height_minus1[j as usize] =
						((j + 1) * sps.pic_height_in_ctbs_y) / nrows - j * sps.pic_height_in_ctbs_y / nrows - 1;
				}
			}

			pps.loop_filter_across_tiles_enabled_flag = r.read_bit()?;
		}

		pps.loop_filter_across_slices_enabled_flag = r.read_bit()?;
		pps.deblocking_filter_control_present_flag = r.read_bit()?;

		if pps.deblocking_filter_control_present_flag {
			pps.deblocking_filter_override_enabled_flag = r.read_bit()?;
			pps.deblocking_filter_disabled_flag = r.read_bit()?;
			if !pps.deblocking_filter_disabled_flag {
				pps.beta_offset_div2 = r.read_se_bounded(-6, 6)?;
				pps.tc_offset_div2 = r.read_se_bounded(-6, 6)?;
			}
		}

		pps.scaling_list_data_present_flag = r.read_bit()?;

		if pps.scaling_list_data_present_flag {
			Self::parse_scaling_list_data(&mut pps.scaling_list, &mut r)?;
		} else {
			for size_id in 0..4 {
				let mut matrix_id = 0;
				while matrix_id < 6 {
					Self::fill_default_scaling_list(&mut pps.scaling_list, size_id, matrix_id);
					let step = if size_id == 3 { 3 } else { 1 };
					matrix_id += step;
				}
			}
		}

		pps.lists_modification_present_flag = r.read_bit()?;
		pps.log2_parallel_merge_level_minus2 = r.read_ue_max(sps.ctb_log2_size_y - 2)?;
		pps.slice_segment_header_extension_present_flag = r.read_bit()?;

		pps.extension_present_flag = r.read_bit()?;
		if pps.extension_present_flag {
			pps.range_extension_flag = r.read_bit()?;

			if pps.range_extension_flag {
				Self::parse_pps_range_extension(&mut pps, sps, &mut r)?;
			}

			let multilayer_extension_flag = r.read_bit()?;
			if multilayer_extension_flag {
				return Err("Multilayer extension is not supported".into());
			}

			let three_d_extension_flag = r.read_bit()?;
			if three_d_extension_flag {
				return Err("3D extension is not supported".into());
			}

			pps.scc_extension_flag = r.read_bit()?;
			if pps.scc_extension_flag {
				Self::parse_pps_scc_extension(&mut pps, sps, &mut r)?;
			}

			r.skip_bits(4)?; // pps_extension_4bits
		}

		tracing::debug!("Parsed PPS({}), NAL size was {}", pps.pic_parameter_set_id, nalu.size);

		if self.active_ppses.keys().len() >= MAX_PPS_COUNT {
			return Err("Broken Data: number of active PPSs > MAX_PPS_COUNT".into());
		}

		let key = pps.pic_parameter_set_id;
		let pps = Rc::new(pps);
		self.active_ppses.remove(&key);
		Ok(self.active_ppses.entry(key).or_insert(pps))
	}

	fn parse_pred_weight_table(hdr: &mut SliceHeader, r: &mut BitReader, sps: &Sps) -> Result<(), String> {
		let pwt = &mut hdr.pred_weight_table;

		pwt.luma_log2_weight_denom = r.read_ue_max(7)?;
		if sps.chroma_array_type != 0 {
			pwt.delta_chroma_log2_weight_denom = r.read_se()?;
			pwt.chroma_log2_weight_denom = (pwt.luma_log2_weight_denom as i32
				+ pwt.delta_chroma_log2_weight_denom as i32)
				.try_into()
				.map_err(|_| String::from("Integer overflow on chroma_log2_weight_denom calculation"))?;
		}

		for i in 0..=usize::from(hdr.num_ref_idx_l0_active_minus1) {
			pwt.luma_weight_l0_flag[i] = r.read_bit()?;
		}

		if sps.chroma_array_type != 0 {
			for i in 0..=usize::from(hdr.num_ref_idx_l0_active_minus1) {
				pwt.chroma_weight_l0_flag[i] = r.read_bit()?;
			}
		}

		for i in 0..=usize::from(hdr.num_ref_idx_l0_active_minus1) {
			if pwt.luma_weight_l0_flag[i] {
				pwt.delta_luma_weight_l0[i] = r.read_se_bounded(-128, 127)?;
				pwt.luma_offset_l0[i] = r.read_se_bounded(-128, 127)?;
			}

			if pwt.chroma_weight_l0_flag[i] {
				for j in 0..2 {
					pwt.delta_chroma_weight_l0[i][j] = r.read_se_bounded(-128, 127)?;
					pwt.delta_chroma_offset_l0[i][j] = r.read_se_bounded(
						-4 * sps.wp_offset_half_range_c as i32,
						4 * sps.wp_offset_half_range_c as i32 - 1,
					)?;
				}
			}
		}

		if hdr.type_.is_b() {
			for i in 0..=usize::from(hdr.num_ref_idx_l1_active_minus1) {
				pwt.luma_weight_l1_flag[i] = r.read_bit()?;
			}

			if sps.chroma_format_idc != 0 {
				for i in 0..=usize::from(hdr.num_ref_idx_l1_active_minus1) {
					pwt.chroma_weight_l1_flag[i] = r.read_bit()?;
				}
			}

			for i in 0..=usize::from(hdr.num_ref_idx_l1_active_minus1) {
				if pwt.luma_weight_l1_flag[i] {
					pwt.delta_luma_weight_l1[i] = r.read_se_bounded(-128, 127)?;
					pwt.luma_offset_l1[i] = r.read_se_bounded(-128, 127)?;
				}

				if pwt.chroma_weight_l1_flag[i] {
					for j in 0..2 {
						pwt.delta_chroma_weight_l1[i][j] = r.read_se_bounded(-128, 127)?;
						pwt.delta_chroma_offset_l1[i][j] = r.read_se_bounded(
							-4 * sps.wp_offset_half_range_c as i32,
							4 * sps.wp_offset_half_range_c as i32 - 1,
						)?;
					}
				}
			}
		}

		Ok(())
	}

	fn parse_ref_pic_lists_modification(hdr: &mut SliceHeader, r: &mut BitReader) -> Result<(), String> {
		let rplm = &mut hdr.ref_pic_list_modification;

		rplm.ref_pic_list_modification_flag_l0 = r.read_bit()?;
		if rplm.ref_pic_list_modification_flag_l0 {
			for _ in 0..=hdr.num_ref_idx_l0_active_minus1 {
				let num_bits = (hdr.num_pic_total_curr as f64).log2().ceil() as _;

				let entry = r.read_bits(num_bits)?;

				if entry > hdr.num_pic_total_curr - 1 {
					return Err(format!(
						"Invalid list_entry_l0 {}, expected at max NumPicTotalCurr - 1: {}",
						entry,
						hdr.num_pic_total_curr - 1
					));
				}

				rplm.list_entry_l0.push(entry);
			}
		}

		if hdr.type_.is_b() {
			rplm.ref_pic_list_modification_flag_l1 = r.read_bit()?;
			if rplm.ref_pic_list_modification_flag_l1 {
				for _ in 0..=hdr.num_ref_idx_l1_active_minus1 {
					let num_bits = (hdr.num_pic_total_curr as f64).log2().ceil() as _;

					let entry = r.read_bits(num_bits)?;

					if entry > hdr.num_pic_total_curr - 1 {
						return Err(format!(
							"Invalid list_entry_l1 {}, expected at max NumPicTotalCurr - 1: {}",
							entry,
							hdr.num_pic_total_curr - 1
						));
					}

					rplm.list_entry_l1.push(entry);
				}
			}
		}

		Ok(())
	}

	/// Further sets default values given `sps` and `pps`.
	pub fn slice_header_set_defaults(hdr: &mut SliceHeader, sps: &Sps, pps: &Pps) {
		// Set some defaults that can't be defined in Default::default().
		hdr.deblocking_filter_disabled_flag = pps.deblocking_filter_disabled_flag;
		hdr.beta_offset_div2 = pps.beta_offset_div2;
		hdr.tc_offset_div2 = pps.tc_offset_div2;
		hdr.loop_filter_across_slices_enabled_flag = pps.loop_filter_across_slices_enabled_flag;
		hdr.curr_rps_idx = sps.num_short_term_ref_pic_sets;
		hdr.use_integer_mv_flag = sps.scc_extension.motion_vector_resolution_control_idc != 0;
	}

	/// Parses a slice header from a slice NALU.
	pub fn parse_slice_header<'a>(&mut self, nalu: Nalu<'a>) -> Result<Slice<'a>, String> {
		if !matches!(
			nalu.header.type_,
			NaluType::TrailN
				| NaluType::TrailR
				| NaluType::TsaN
				| NaluType::TsaR
				| NaluType::StsaN
				| NaluType::StsaR
				| NaluType::RadlN
				| NaluType::RadlR
				| NaluType::RaslN
				| NaluType::RaslR
				| NaluType::BlaWLp
				| NaluType::BlaWRadl
				| NaluType::BlaNLp
				| NaluType::IdrWRadl
				| NaluType::IdrNLp
				| NaluType::CraNut,
		) {
			return Err(format!(
				"Invalid NALU type: {:?} is not a slice NALU",
				nalu.header.type_
			));
		}

		let data = nalu.as_ref();
		let nalu_header = &nalu.header;
		let hdr_len = nalu_header.len();
		// Skip the header
		let mut r = BitReader::new(&data[hdr_len..], true);

		let mut hdr = SliceHeader {
			first_slice_segment_in_pic_flag: r.read_bit()?,
			..Default::default()
		};

		if nalu.header.type_.is_irap() {
			hdr.no_output_of_prior_pics_flag = r.read_bit()?;
		}

		hdr.pic_parameter_set_id = r.read_ue_max(63)?;

		let pps = self.get_pps(hdr.pic_parameter_set_id).ok_or::<String>(format!(
			"Could not get PPS for pic_parameter_set_id {}",
			hdr.pic_parameter_set_id
		))?;

		let sps = &pps.sps;

		Self::slice_header_set_defaults(&mut hdr, sps, pps);

		if !hdr.first_slice_segment_in_pic_flag {
			if pps.dependent_slice_segments_enabled_flag {
				hdr.dependent_slice_segment_flag = r.read_bit()?;
			}

			let num_bits = (sps.pic_size_in_ctbs_y as f64).log2().ceil() as _;
			hdr.segment_address = r.read_bits(num_bits)?;

			if hdr.segment_address > sps.pic_size_in_ctbs_y - 1 {
				return Err(format!("Invalid slice_segment_address {}", hdr.segment_address));
			}
		}

		if !hdr.dependent_slice_segment_flag {
			r.skip_bits(usize::from(pps.num_extra_slice_header_bits))?;

			let slice_type: u32 = r.read_ue()?;
			hdr.type_ = SliceType::try_from(slice_type)?;

			if pps.output_flag_present_flag {
				hdr.pic_output_flag = r.read_bit()?;
			}

			if sps.separate_colour_plane_flag {
				hdr.colour_plane_id = r.read_bits(2)?;
			}

			if !matches!(nalu_header.type_, NaluType::IdrWRadl | NaluType::IdrNLp) {
				let num_bits = usize::from(sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
				hdr.pic_order_cnt_lsb = r.read_bits(num_bits)?;

				if u32::from(hdr.pic_order_cnt_lsb) > 2u32.pow(u32::from(sps.log2_max_pic_order_cnt_lsb_minus4 + 4)) {
					return Err(format!("Invalid pic_order_cnt_lsb {}", hdr.pic_order_cnt_lsb));
				}

				hdr.short_term_ref_pic_set_sps_flag = r.read_bit()?;

				if !hdr.short_term_ref_pic_set_sps_flag {
					let epb_before = r.num_epb();
					let bits_left_before = r.num_bits_left();

					let st_rps_idx = sps.num_short_term_ref_pic_sets;

					Self::parse_short_term_ref_pic_set(sps, &mut hdr.short_term_ref_pic_set, &mut r, st_rps_idx)?;

					hdr.st_rps_bits = ((bits_left_before - r.num_bits_left()) - 8 * (r.num_epb() - epb_before)) as u32;
				} else if sps.num_short_term_ref_pic_sets > 1 {
					let num_bits = (sps.num_short_term_ref_pic_sets as f64).log2().ceil() as _;
					hdr.short_term_ref_pic_set_idx = r.read_bits(num_bits)?;

					if hdr.short_term_ref_pic_set_idx > sps.num_short_term_ref_pic_sets - 1 {
						return Err(format!(
							"Invalid short_term_ref_pic_set_idx {}",
							hdr.short_term_ref_pic_set_idx
						));
					}
				}

				if hdr.short_term_ref_pic_set_sps_flag {
					hdr.curr_rps_idx = hdr.short_term_ref_pic_set_idx;
				}

				if sps.long_term_ref_pics_present_flag {
					if sps.num_long_term_ref_pics_sps > 0 {
						hdr.num_long_term_sps = r.read_ue_max(u32::from(sps.num_long_term_ref_pics_sps))?;
					}

					hdr.num_long_term_pics =
						r.read_ue_max(MAX_LONG_TERM_REF_PIC_SETS as u32 - u32::from(hdr.num_long_term_sps))?;

					let num_lt = hdr.num_long_term_sps + hdr.num_long_term_pics;
					for i in 0..usize::from(num_lt) {
						// The variables `PocLsbLt[ i ]` and `UsedByCurrPicLt[ i ]` are derived as follows:
						//
						// – If i is less than num_long_term_sps, `PocLsbLt[ i ]` is set equal to
						// lt_ref_pic_poc_lsb_sps[ `lt_idx_sps[ i ]` ] and `UsedByCurrPicLt[ i ]` is set equal
						// to used_by_curr_pic_lt_sps_flag[ `lt_idx_sps[ i ]` ].
						//
						// – Otherwise, `PocLsbLt[ i ]`
						// is set equal to `poc_lsb_lt[ i ]` and `UsedByCurrPicLt[ i ]` is set equal to
						// `used_by_curr_pic_lt_flag[ i ]`.
						if i < usize::from(hdr.num_long_term_sps) {
							if sps.num_long_term_ref_pics_sps > 1 {
								let num_bits = (sps.num_long_term_ref_pics_sps as f64).log2().ceil() as _;

								hdr.lt_idx_sps[i] = r.read_bits(num_bits)?;

								if hdr.lt_idx_sps[i] > sps.num_long_term_ref_pics_sps - 1 {
									return Err(format!("Invalid lt_idx_sps[{}] {}", i, hdr.lt_idx_sps[i]));
								}
							}

							hdr.poc_lsb_lt[i] = sps.lt_ref_pic_poc_lsb_sps[usize::from(hdr.lt_idx_sps[i])];
							hdr.used_by_curr_pic_lt[i] =
								sps.used_by_curr_pic_lt_sps_flag[usize::from(hdr.lt_idx_sps[i])];
						} else {
							let num_bits = usize::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4;
							hdr.poc_lsb_lt[i] = r.read_bits(num_bits)?;
							hdr.used_by_curr_pic_lt[i] = r.read_bit()?;
						}

						hdr.delta_poc_msb_present_flag[i] = r.read_bit()?;
						if hdr.delta_poc_msb_present_flag[i] {
							// The value of `delta_poc_msb_cycle_lt[ i ]` shall be
							// in the range of 0 to 2(32 −
							// log2_max_pic_order_cnt_lsb_minus4 − 4 ),
							// inclusive. When `delta_poc_msb_cycle_lt[ i ]` is
							// not present, it is inferred to be equal to 0.
							let max = 2u32.pow(32 - u32::from(sps.log2_max_pic_order_cnt_lsb_minus4) - 4);
							hdr.delta_poc_msb_cycle_lt[i] = r.read_ue_max(max)?;
						}
						// Equation 7-52 (simplified)
						if i != 0 && i != usize::from(hdr.num_long_term_sps) {
							hdr.delta_poc_msb_cycle_lt[i] += hdr.delta_poc_msb_cycle_lt[i - 1];
						}
					}
				}

				if sps.temporal_mvp_enabled_flag {
					hdr.temporal_mvp_enabled_flag = r.read_bit()?;
				}
			}

			if sps.sample_adaptive_offset_enabled_flag {
				hdr.sao_luma_flag = r.read_bit()?;
				if sps.chroma_array_type != 0 {
					hdr.sao_chroma_flag = r.read_bit()?;
				}
			}

			if hdr.type_.is_p() || hdr.type_.is_b() {
				hdr.num_ref_idx_active_override_flag = r.read_bit()?;
				if hdr.num_ref_idx_active_override_flag {
					hdr.num_ref_idx_l0_active_minus1 = r.read_ue_max(MAX_REF_IDX_ACTIVE - 1)?;
					if hdr.type_.is_b() {
						hdr.num_ref_idx_l1_active_minus1 = r.read_ue_max(MAX_REF_IDX_ACTIVE - 1)?;
					}
				} else {
					hdr.num_ref_idx_l0_active_minus1 = pps.num_ref_idx_l0_default_active_minus1;
					hdr.num_ref_idx_l1_active_minus1 = pps.num_ref_idx_l1_default_active_minus1;
				}

				// 7-57
				let mut num_pic_total_curr = 0;
				let rps = if hdr.short_term_ref_pic_set_sps_flag {
					sps.short_term_ref_pic_set
						.get(usize::from(hdr.curr_rps_idx))
						.ok_or::<String>("Invalid RPS".into())?
				} else {
					&hdr.short_term_ref_pic_set
				};

				for i in 0..usize::from(rps.num_negative_pics) {
					if rps.used_by_curr_pic_s0[i] {
						num_pic_total_curr += 1;
					}
				}

				for i in 0..usize::from(rps.num_positive_pics) {
					if rps.used_by_curr_pic_s1[i] {
						num_pic_total_curr += 1;
					}
				}

				for i in 0..usize::from(hdr.num_long_term_sps + hdr.num_long_term_pics) {
					if hdr.used_by_curr_pic_lt[i] {
						num_pic_total_curr += 1;
					}
				}

				if pps.scc_extension.curr_pic_ref_enabled_flag {
					num_pic_total_curr += 1;
				}

				hdr.num_pic_total_curr = num_pic_total_curr;

				if pps.lists_modification_present_flag && hdr.num_pic_total_curr > 1 {
					Self::parse_ref_pic_lists_modification(&mut hdr, &mut r)?;
				}

				if hdr.type_.is_b() {
					hdr.mvd_l1_zero_flag = r.read_bit()?;
				}

				if pps.cabac_init_present_flag {
					hdr.cabac_init_flag = r.read_bit()?;
				}

				if hdr.temporal_mvp_enabled_flag {
					if hdr.type_.is_b() {
						hdr.collocated_from_l0_flag = r.read_bit()?;
					}

					if (hdr.collocated_from_l0_flag && hdr.num_ref_idx_l0_active_minus1 > 0)
						|| (!hdr.collocated_from_l0_flag && hdr.num_ref_idx_l1_active_minus1 > 0)
					{
						let max = if (hdr.type_.is_p() || hdr.type_.is_b()) && hdr.collocated_from_l0_flag {
							hdr.num_ref_idx_l0_active_minus1
						} else if hdr.type_.is_b() && !hdr.collocated_from_l0_flag {
							hdr.num_ref_idx_l1_active_minus1
						} else {
							return Err("Invalid value for collocated_ref_idx".into());
						};

						{
							hdr.collocated_ref_idx = r.read_ue_max(u32::from(max))?;
						}
					}
				}

				if (pps.weighted_pred_flag && hdr.type_.is_p()) || (pps.weighted_bipred_flag && hdr.type_.is_b()) {
					Self::parse_pred_weight_table(&mut hdr, &mut r, sps)?;
				}

				hdr.five_minus_max_num_merge_cand = r.read_ue()?;

				if sps.scc_extension.motion_vector_resolution_control_idc == 2 {
					hdr.use_integer_mv_flag = r.read_bit()?;
				}
			}

			hdr.qp_delta = r.read_se_bounded(-87, 77)?;

			let slice_qp_y = (26 + pps.init_qp_minus26 + hdr.qp_delta) as i32;
			if slice_qp_y < -(pps.qp_bd_offset_y as i32) || slice_qp_y > 51 {
				return Err(format!("Invalid slice_qp_delta: {}", hdr.qp_delta));
			}

			if pps.slice_chroma_qp_offsets_present_flag {
				hdr.cb_qp_offset = r.read_se_bounded(-12, 12)?;

				let qp_offset = pps.cb_qp_offset + hdr.cb_qp_offset;
				if !(-12..=12).contains(&qp_offset) {
					return Err(format!("Invalid value for slice_cb_qp_offset: {}", hdr.cb_qp_offset));
				}

				hdr.cr_qp_offset = r.read_se_bounded(-12, 12)?;

				let qp_offset = pps.cr_qp_offset + hdr.cr_qp_offset;
				if !(-12..=12).contains(&qp_offset) {
					return Err(format!("Invalid value for slice_cr_qp_offset: {}", hdr.cr_qp_offset));
				}
			}

			if pps.scc_extension.slice_act_qp_offsets_present_flag {
				hdr.slice_act_y_qp_offset = r.read_se_bounded(-12, 12)?;
				hdr.slice_act_cb_qp_offset = r.read_se_bounded(-12, 12)?;
				hdr.slice_act_cr_qp_offset = r.read_se_bounded(-12, 12)?;
			}

			if pps.range_extension.chroma_qp_offset_list_enabled_flag {
				hdr.cu_chroma_qp_offset_enabled_flag = r.read_bit()?;
			}

			if pps.deblocking_filter_override_enabled_flag {
				hdr.deblocking_filter_override_flag = r.read_bit()?;
			}

			if hdr.deblocking_filter_override_flag {
				hdr.deblocking_filter_disabled_flag = r.read_bit()?;
				if !hdr.deblocking_filter_disabled_flag {
					hdr.beta_offset_div2 = r.read_se_bounded(-6, 6)?;
					hdr.tc_offset_div2 = r.read_se_bounded(-6, 6)?;
				}
			}

			if pps.loop_filter_across_slices_enabled_flag
				&& (hdr.sao_luma_flag || hdr.sao_chroma_flag || !hdr.deblocking_filter_disabled_flag)
			{
				hdr.loop_filter_across_slices_enabled_flag = r.read_bit()?;
			}
		}

		if pps.tiles_enabled_flag || pps.entropy_coding_sync_enabled_flag {
			let max = if !pps.tiles_enabled_flag && pps.entropy_coding_sync_enabled_flag {
				sps.pic_height_in_ctbs_y - 1
			} else if pps.tiles_enabled_flag && !pps.entropy_coding_sync_enabled_flag {
				(u32::from(pps.num_tile_columns_minus1) + 1) * (u32::from(pps.num_tile_rows_minus1) + 1) - 1
			} else {
				(u32::from(pps.num_tile_columns_minus1) + 1) * sps.pic_height_in_ctbs_y - 1
			};

			hdr.num_entry_point_offsets = r.read_ue_max(max)?;
			if hdr.num_entry_point_offsets > 0 {
				hdr.offset_len_minus1 = r.read_ue_max(31)?;
				// One per CTB row or tile, so a 4K picture with wavefronts has
				// more than a fixed array would hold.
				let num_bits = usize::from(hdr.offset_len_minus1 + 1);
				hdr.entry_point_offset_minus1 = (0..hdr.num_entry_point_offsets)
					.map(|_| r.read_bits(num_bits))
					.collect::<Result<_, _>>()?;
			}
		}

		if pps.slice_segment_header_extension_present_flag {
			let segment_header_extension_length = r.read_ue_max(256)?;
			for _ in 0..segment_header_extension_length {
				r.skip_bits(8)?; // slice_segment_header_extension_data_byte[i]
			}
		}

		// byte_alignment()
		r.skip_bits(1)?; // Alignment bit
		let num_bits = r.num_bits_left() % 8;
		r.skip_bits(num_bits)?;

		let epb = r.num_epb();
		hdr.header_bit_size = ((nalu.size - epb) * 8 - r.num_bits_left()) as u32;

		hdr.n_emulation_prevention_bytes = epb as u32;

		tracing::debug!("Parsed slice {:?}, NAL size was {}", nalu_header.type_, nalu.size);

		Ok(Slice { header: hdr, nalu })
	}

	/// Returns a previously parsed vps given `vps_id`, if any.
	pub fn get_vps(&self, vps_id: u8) -> Option<&Rc<Vps>> {
		self.active_vpses.get(&vps_id)
	}

	/// Returns a previously parsed sps given `sps_id`, if any.
	pub fn get_sps(&self, sps_id: u8) -> Option<&Rc<Sps>> {
		self.active_spses.get(&sps_id)
	}

	/// Returns a previously parsed pps given `pps_id`, if any.
	pub fn get_pps(&self, pps_id: u8) -> Option<&Rc<Pps>> {
		self.active_ppses.get(&pps_id)
	}
}
