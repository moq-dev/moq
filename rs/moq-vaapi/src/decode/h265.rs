//! The H.265 decoder.
//!
//! Ported from discord/cros-codecs's `decoder/stateless/h265.rs` and
//! `decoder/stateless/h265/vaapi.rs` (BSD-3-Clause), driving libva directly the
//! way the H.264 decoder beside it does. The reference picture set (8.3.2),
//! reference list construction (8.3.4), and DPB output (C.5.2) follow the
//! specification as cros-codecs does, with the screen content and range
//! extension paths left out: a sequence that needs them is rejected up front.
//!
//! Output follows C.5.2's bumping, which works from the SPS's reorder limit
//! rather than the DPB size, so a stream coded without reordering comes out the
//! moment each picture decodes.

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _};

use super::{create_buffer, run, Format, Handle, Sequence};
use crate::codec::h265::dpb::{Dpb, DpbEntry};
use crate::codec::h265::parser::{Nalu, NaluType, Parser, Pps, ScalingLists, Slice, SliceHeader, Sps};
use crate::codec::h265::picture::{PictureData, Reference};
use crate::codec::Codec;
use crate::{
	BufferType, Display, HevcLongSliceFlags, HevcPicFields, HevcSliceParsingFields, IQMatrix, IQMatrixBufferHEVC,
	Picture, PictureHEVC, PictureParameter, PictureParameterBufferHEVC, SliceParameter, SliceParameterBufferHEVC,
	VAProfile, VA_INVALID_ID, VA_PICTURE_HEVC_INVALID, VA_PICTURE_HEVC_LONG_TERM_REFERENCE,
	VA_PICTURE_HEVC_RPS_LT_CURR, VA_PICTURE_HEVC_RPS_ST_CURR_AFTER, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE,
	VA_SLICE_DATA_FLAG_ALL,
};

/// The most pictures a DPB holds (A.4.2).
const MAX_DPB_SIZE: usize = 16;

/// The most reference pictures a VA picture parameter buffer lists.
const MAX_VA_REFERENCES: usize = 15;

/// The H.265 decoder.
pub(super) struct Decoder {
	parser: Parser,
	dpb: Dpb<Handle>,
	/// The SPS of the last picture begun, whose limits drive bumping (C.5.2).
	sps: Option<Rc<Sps>>,
	/// PrevTid0Pic of 8.3.1, which the next picture's order count derives from.
	prev_tid0_pic: Option<PictureData>,
	first_picture_in_bitstream: bool,
	first_picture_after_eos: bool,
	/// NoRaslOutputFlag of the last IRAP picture, which decides whether the
	/// RASL pictures after it can be decoded at all.
	irap_no_rasl_output_flag: bool,
	/// The header a dependent slice segment inherits.
	last_independent_slice_header: Option<SliceHeader>,
	/// VA state for the active sequence, rebuilt when the SPS changes something
	/// the config, context, or surface pool depends on.
	pub(super) sequence: Option<Sequence>,
	/// Pictures bumped out of the DPB, in output order, awaiting download.
	pub(super) ready: Vec<Handle>,
	/// Drop order: everything above borrows the display.
	display: Arc<Display>,
}

/// The picture being assembled, held across the slices that make it up.
struct CurrentPicture {
	pic: PictureData,
	/// The SPS the picture was begun with.
	sps: Rc<Sps>,
	/// The reference picture set derived for it (8.3.2).
	rps: RefPicSet,
	/// The DPB's reference pictures, in the order the picture parameters list
	/// them, which is what a slice's reference lists index into.
	references: Vec<DpbEntry<Handle>>,
	handle: Handle,
	picture: Picture<crate::PictureNew, Handle>,
	/// The last slice's parameters and data, held back one slice so the final
	/// one can be marked as the last of the picture.
	last_slice: Option<(SliceParameterBufferHEVC, Vec<u8>)>,
}

/// The reference picture set of one picture (8.3.2). A missing entry is a
/// reference the stream names but the DPB does not hold.
#[derive(Default)]
struct RefPicSet {
	st_curr_before: Vec<Option<DpbEntry<Handle>>>,
	st_curr_after: Vec<Option<DpbEntry<Handle>>>,
	st_foll: Vec<Option<DpbEntry<Handle>>>,
	lt_curr: Vec<Option<DpbEntry<Handle>>>,
	lt_foll: Vec<Option<DpbEntry<Handle>>>,
}

impl RefPicSet {
	fn lists(&self) -> [&[Option<DpbEntry<Handle>>]; 5] {
		[
			&self.st_curr_before,
			&self.st_curr_after,
			&self.st_foll,
			&self.lt_curr,
			&self.lt_foll,
		]
	}

	/// 8.3.4: the reference picture lists of a slice, each as long as the
	/// slice's active reference count.
	fn ref_pic_lists(&self, hdr: &SliceHeader) -> anyhow::Result<[Vec<Option<DpbEntry<Handle>>>; 2]> {
		if !hdr.type_.is_p() && !hdr.type_.is_b() {
			return Ok(Default::default());
		}

		// RefPicListTemp0 and RefPicListTemp1 (8-8, 8-10) repeat these until
		// they are long enough, so an entry is picked modulo their length.
		let before = || self.st_curr_before.iter();
		let after = || self.st_curr_after.iter();
		let long = || self.lt_curr.iter();
		let temp0: Vec<_> = before().chain(after()).chain(long()).collect();
		if temp0.is_empty() {
			bail!("invalid stream: an inter slice whose reference picture set holds nothing to predict from");
		}
		let temp1: Vec<_> = after().chain(before()).chain(long()).collect();

		let rplm = &hdr.ref_pic_list_modification;
		let build = |temp: &[&Option<DpbEntry<Handle>>], active_minus1: u8, modified: bool, entries: &[u32]| {
			(0..=usize::from(active_minus1))
				.map(|index| {
					// 8-9 and 8-11.
					let index = if modified {
						entries.get(index).map_or(index, |&entry| entry as usize)
					} else {
						index
					};
					temp[index % temp.len()].clone()
				})
				.collect::<Vec<_>>()
		};

		let list0 = build(
			&temp0,
			hdr.num_ref_idx_l0_active_minus1,
			rplm.ref_pic_list_modification_flag_l0,
			&rplm.list_entry_l0,
		);
		let list1 = if hdr.type_.is_b() {
			build(
				&temp1,
				hdr.num_ref_idx_l1_active_minus1,
				rplm.ref_pic_list_modification_flag_l1,
				&rplm.list_entry_l1,
			)
		} else {
			Vec::new()
		};

		Ok([list0, list1])
	}
}

impl Decoder {
	pub(super) fn new(display: Arc<Display>) -> Self {
		Self {
			parser: Parser::default(),
			dpb: Dpb::default(),
			sps: None,
			prev_tid0_pic: None,
			first_picture_in_bitstream: true,
			first_picture_after_eos: true,
			irap_no_rasl_output_flag: false,
			last_independent_slice_header: None,
			sequence: None,
			ready: Vec::new(),
			display,
		}
	}

	/// Decodes one access unit into the ready queue, returning how many NAL units it held.
	pub(super) fn submit(&mut self, access_unit: &[u8], timestamp: u64) -> anyhow::Result<usize> {
		let mut cursor = Cursor::new(access_unit);
		let mut current: Option<CurrentPicture> = None;
		let mut nalus = 0usize;

		// `Nalu::next` reports the end of the buffer as an error, so a truncated
		// unit is indistinguishable from a complete one and simply stops us.
		while let Ok(nalu) = Nalu::next(&mut cursor) {
			nalus += 1;
			match nalu.header.type_ {
				NaluType::VpsNut => {
					self.parser.parse_vps(&nalu).map_err(|e| anyhow!("parse VPS: {e}"))?;
				}
				NaluType::SpsNut => {
					self.parser.parse_sps(&nalu).map_err(|e| anyhow!("parse SPS: {e}"))?;
				}
				NaluType::PpsNut => {
					self.parser.parse_pps(&nalu).map_err(|e| anyhow!("parse PPS: {e}"))?;
				}
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
				| NaluType::CraNut => {
					let mut slice = self
						.parser
						.parse_slice_header(nalu)
						.map_err(|e| anyhow!("parse slice header: {e}"))?;

					if slice.header.dependent_slice_segment_flag {
						let header = self
							.last_independent_slice_header
							.clone()
							.context("invalid stream: a dependent slice segment with no independent one before it")?;
						slice.replace_header(header).map_err(|e| anyhow!("{e}"))?;
					} else {
						self.last_independent_slice_header = Some(slice.header.clone());
					}

					// An access unit normally holds one picture, but nothing
					// forces that: the first segment of a picture ends the one in
					// flight.
					if slice.header.first_slice_segment_in_pic_flag {
						if let Some(picture) = current.take() {
							self.finish_picture(picture)?;
						}
					}
					if current.is_none() {
						current = self.begin_picture(&slice, timestamp)?;
					}
					// `None` is a RASL picture that cannot be decoded, skipped
					// one slice at a time.
					if let Some(picture) = current.as_mut() {
						self.decode_slice(picture, &slice)?;
					}
				}
				NaluType::EosNut => {
					if let Some(picture) = current.take() {
						self.finish_picture(picture)?;
					}
					// What follows decodes without reference to anything before,
					// so nothing before has to wait for it.
					self.drain();
					self.first_picture_after_eos = true;
				}
				NaluType::EobNut => {
					if let Some(picture) = current.take() {
						self.finish_picture(picture)?;
					}
					self.drain();
					self.first_picture_in_bitstream = true;
				}
				_ => {}
			}
		}

		// Finishing here rather than on the next unit's first slice keeps the
		// hardware from holding a half-submitted picture between calls.
		if let Some(picture) = current.take() {
			self.finish_picture(picture)?;
		}

		Ok(nalus)
	}

	/// Moves the DPB to the ready queue and forgets the state the next picture
	/// would otherwise be derived against, so it starts the stream over.
	pub(super) fn reset(&mut self) {
		self.drain();
		self.prev_tid0_pic = None;
		self.first_picture_after_eos = true;
		self.last_independent_slice_header = None;
	}

	/// Moves everything left in the DPB to the ready queue.
	fn drain(&mut self) {
		self.ready.extend(self.dpb.drain().into_iter().map(|entry| entry.1));
		// What is left is reference pictures already output.
		self.dpb.clear();
	}

	/// Applies `sps`, rebuilding the VA config, context, and surface pool if it
	/// changes anything they depend on.
	fn apply_sps(&mut self, sps: &Rc<Sps>) -> anyhow::Result<()> {
		let format = format(sps)?;
		self.dpb.set_max_num_pics(format.max_dpb_frames);
		self.sps = Some(Rc::clone(sps));

		if self.sequence.as_ref().map(|sequence| &sequence.format) == Some(&format) {
			return Ok(());
		}

		// Surfaces are about to change shape, so let everything decoded against
		// the old sequence out first. Those handles keep their own surfaces (and
		// through them the old pool) alive until they are downloaded.
		self.drain();

		self.sequence = Some(Sequence::new(&self.display, format)?);
		Ok(())
	}

	/// Starts a picture: derives its order count and reference picture set,
	/// makes room in the DPB, and hands the driver the picture parameters.
	///
	/// Returns `None` for a RASL picture whose IRAP picture started the stream
	/// over, which may refer to pictures that were never decoded (8.1.3).
	fn begin_picture(&mut self, slice: &Slice, timestamp: u64) -> anyhow::Result<Option<CurrentPicture>> {
		let pps = Rc::clone(
			self.parser
				.get_pps(slice.header.pic_parameter_set_id)
				.context("slice refers to an unknown PPS")?,
		);
		let sps = Rc::clone(&pps.sps);
		self.apply_sps(&sps)?;

		let max_pic_order_cnt_lsb = 1 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
		let pic = PictureData::new_from_slice(
			slice,
			self.first_picture_in_bitstream,
			self.first_picture_after_eos,
			self.prev_tid0_pic.as_ref(),
			max_pic_order_cnt_lsb,
		);
		self.first_picture_in_bitstream = false;
		self.first_picture_after_eos = false;

		if pic.nalu_type.is_irap() {
			self.irap_no_rasl_output_flag = pic.no_rasl_output_flag;
		} else if pic.nalu_type.is_rasl() && self.irap_no_rasl_output_flag {
			tracing::debug!("skipping RASL picture POC {}", pic.pic_order_cnt_val);
			return Ok(None);
		}

		let rps = self.derive_rps(slice, &sps, &pic, max_pic_order_cnt_lsb);
		self.update_dpb_before_decoding(&pic, &sps);

		let references = self.dpb.get_all_references();
		if references.len() > MAX_VA_REFERENCES {
			bail!(
				"invalid stream: {} reference pictures, more than the {MAX_VA_REFERENCES} a picture can use",
				references.len()
			);
		}

		// `apply_sps` above either kept the sequence or built a new one.
		let sequence = self.sequence.as_ref().expect("sequence built by apply_sps");
		let context = Rc::clone(&sequence.context);
		let handle = sequence.handle(timestamp)?;

		let mut picture = Picture::new(timestamp, Rc::clone(&context), handle.clone());
		let pic_param = build_pic_param(&pic, handle.surface.id(), &references, &rps, &sps, &pps);
		picture.add_buffer(create_buffer(&context, pic_param)?);
		if let Some(lists) = scaling_lists(&sps, &pps) {
			picture.add_buffer(create_buffer(&context, build_iq_matrix(lists))?);
		}

		tracing::trace!("starting picture POC {}", pic.pic_order_cnt_val);
		Ok(Some(CurrentPicture {
			pic,
			sps,
			rps,
			references,
			handle,
			picture,
			last_slice: None,
		}))
	}

	/// 8.3.2: derives the current picture's reference picture set and marks the
	/// DPB to match it.
	fn derive_rps(&mut self, slice: &Slice, sps: &Sps, pic: &PictureData, max_pic_order_cnt_lsb: i32) -> RefPicSet {
		let hdr = &slice.header;

		if pic.nalu_type.is_irap() && pic.no_rasl_output_flag {
			self.dpb.mark_all_as_unused_for_ref();
		}

		let mut rps = RefPicSet::default();
		if !pic.nalu_type.is_idr() {
			let poc = pic.pic_order_cnt_val;

			// 8-5: the long-term pictures, found by the low bits of their order
			// count unless the stream sent the high bits too.
			let mut lt_curr = Vec::new();
			let mut lt_foll = Vec::new();
			for i in 0..usize::from(hdr.num_long_term_sps + hdr.num_long_term_pics) {
				let mut poc_lt = hdr.poc_lsb_lt[i] as i32;
				let msb = hdr.delta_poc_msb_present_flag[i];
				if msb {
					poc_lt += poc
						.wrapping_sub((hdr.delta_poc_msb_cycle_lt[i] as i32).wrapping_mul(max_pic_order_cnt_lsb))
						.wrapping_sub(poc & (max_pic_order_cnt_lsb - 1));
				}
				if hdr.used_by_curr_pic_lt[i] {
					lt_curr.push((poc_lt, msb));
				} else {
					lt_foll.push((poc_lt, msb));
				}
			}

			let find_long_term = |&(poc, msb): &(i32, bool)| {
				let found = if msb {
					self.dpb.find_ref_by_poc(poc)
				} else {
					self.dpb.find_ref_by_poc_masked(poc, max_pic_order_cnt_lsb - 1)
				};
				if found.is_none() {
					tracing::warn!("no long-term reference picture with POC {poc}");
				}
				found
			};
			rps.lt_curr = lt_curr.iter().map(find_long_term).collect();
			rps.lt_foll = lt_foll.iter().map(find_long_term).collect();
			for entry in rps.lt_curr.iter().chain(&rps.lt_foll).flatten() {
				entry.0.borrow_mut().set_reference(Reference::LongTerm);
			}

			// 8-6: the short-term pictures, after the long-term ones are marked
			// so a picture is never both.
			let st = if hdr.curr_rps_idx == sps.num_short_term_ref_pic_sets {
				&hdr.short_term_ref_pic_set
			} else {
				&sps.short_term_ref_pic_set[usize::from(hdr.curr_rps_idx)]
			};
			let find_short_term = |poc: i32| {
				let found = self.dpb.find_short_term_ref_by_poc(poc);
				if found.is_none() {
					tracing::warn!("no short-term reference picture with POC {poc}");
				}
				found
			};
			for i in 0..usize::from(st.num_negative_pics) {
				let entry = find_short_term(poc.wrapping_add(st.delta_poc_s0[i]));
				if st.used_by_curr_pic_s0[i] {
					rps.st_curr_before.push(entry);
				} else {
					rps.st_foll.push(entry);
				}
			}
			for i in 0..usize::from(st.num_positive_pics) {
				let entry = find_short_term(poc.wrapping_add(st.delta_poc_s1[i]));
				if st.used_by_curr_pic_s1[i] {
					rps.st_curr_after.push(entry);
				} else {
					rps.st_foll.push(entry);
				}
			}
		}

		// Every reference picture the set leaves out is no longer one.
		for entry in self.dpb.entries() {
			let kept = rps
				.lists()
				.iter()
				.any(|list| list.iter().flatten().any(|kept| Rc::ptr_eq(&kept.0, &entry.0)));
			if !kept {
				entry.0.borrow_mut().set_reference(Reference::None);
			}
		}

		rps
	}

	/// C.5.2.2: empties the DPB at a picture that starts the stream over, and
	/// otherwise outputs and removes pictures until the current one fits.
	fn update_dpb_before_decoding(&mut self, pic: &PictureData, sps: &Sps) {
		if pic.nalu_type.is_irap() && pic.no_rasl_output_flag {
			if pic.no_output_of_prior_pics_flag {
				self.dpb.clear();
			} else {
				self.drain();
			}
		} else {
			self.dpb.remove_unused();
			while self.dpb.needs_bumping(sps) {
				let Some(entry) = self.dpb.bump(false) else {
					break;
				};
				self.ready.push(entry.1);
			}
		}
	}

	/// Hands the driver one slice. The slice before it goes now; this one waits
	/// for the next, or for [`Decoder::finish_picture`] to mark it as the last.
	fn decode_slice(&mut self, current: &mut CurrentPicture, slice: &Slice) -> anyhow::Result<()> {
		// A slice may refer to a different PPS than the one that started the
		// picture, as long as it names the same sequence.
		let pps = Rc::clone(
			self.parser
				.get_pps(slice.header.pic_parameter_set_id)
				.context("slice refers to an unknown PPS")?,
		);
		if format(&pps.sps)? != format(&current.sps)? {
			bail!("invalid stream: the sequence changed between slices of one picture");
		}

		let [list0, list1] = current.rps.ref_pic_lists(&slice.header)?;
		let slice_param = build_slice_param(slice, &pps.sps, &list0, &list1, &current.references)?;

		let context = &self.sequence.as_ref().expect("sequence built by apply_sps").context;
		if let Some((param, data)) = current.last_slice.take() {
			add_slice(&mut current.picture, context, param, data)?;
		}
		current.last_slice = Some((slice_param, slice.nalu.as_ref().to_vec()));

		Ok(())
	}

	/// Submits the picture to the hardware, stores it in the DPB, and outputs
	/// what the store leaves over the reorder limit (C.5.2.3).
	fn finish_picture(&mut self, current: CurrentPicture) -> anyhow::Result<()> {
		let CurrentPicture {
			pic,
			handle,
			mut picture,
			last_slice,
			..
		} = current;

		let context = &self.sequence.as_ref().expect("sequence built by apply_sps").context;
		if let Some((mut param, data)) = last_slice {
			param.set_as_last();
			add_slice(&mut picture, context, param, data)?;
		}
		run(picture)?;
		tracing::trace!("decoded picture POC {}", pic.pic_order_cnt_val);

		// 8.3.1
		if pic.valid_for_prev_tid0_pic {
			self.prev_tid0_pic = Some(pic.clone());
		}

		self.dpb
			.store_picture(Rc::new(RefCell::new(pic)), handle)
			.map_err(|e| anyhow!("store picture in DPB: {e}"))?;

		let sps = self.sps.as_ref().expect("SPS applied by begin_picture");
		while self.dpb.needs_additional_bumping(sps) {
			let Some(entry) = self.dpb.bump(false) else {
				break;
			};
			self.ready.push(entry.1);
		}

		Ok(())
	}
}

/// Checks that `sps` describes something this decoder handles, and returns what
/// its VA state is built for.
fn format(sps: &Sps) -> anyhow::Result<Format> {
	// A.3: Main, Main 10, and Main Still Picture, either named or claimed
	// through the compatibility flags, which is how a stream marks itself
	// decodable by a Main decoder.
	let ptl = &sps.profile_tier_level;
	let main =
		matches!(ptl.general_profile_idc, 1..=3) || ptl.general_profile_compatibility_flag[1..=3].contains(&true);
	if !main {
		bail!(
			"only Main profile H.265 is supported, got general_profile_idc {}",
			ptl.general_profile_idc
		);
	}
	if sps.chroma_format_idc != 1 {
		bail!(
			"only 4:2:0 chroma is supported, got chroma_format_idc {}",
			sps.chroma_format_idc
		);
	}
	if sps.bit_depth_luma_minus8 != 0 || sps.bit_depth_chroma_minus8 != 0 {
		bail!(
			"only 8-bit samples are supported, got {}-bit luma and {}-bit chroma",
			sps.bit_depth_luma_minus8 + 8,
			sps.bit_depth_chroma_minus8 + 8
		);
	}
	// H.265 codes interlaced video as a sequence of field pictures, each of
	// which would otherwise come out as a frame of half the height.
	if sps.vui_parameters_present_flag && sps.vui_parameters.field_seq_flag {
		bail!("interlaced H.265 is not supported");
	}

	// The conformance window is in chroma samples, two luma samples each way
	// for 4:2:0.
	let (crop_right, crop_bottom) = if sps.conformance_window_flag {
		if sps.conf_win_left_offset != 0 || sps.conf_win_top_offset != 0 {
			bail!("left and top H.265 cropping is not supported");
		}
		(sps.conf_win_right_offset, sps.conf_win_bottom_offset)
	} else {
		(0, 0)
	};
	let coded = (u32::from(sps.width()), u32::from(sps.height()));
	let width = crop_right
		.checked_mul(2)
		.and_then(|crop| coded.0.checked_sub(crop))
		.context("invalid horizontal H.265 crop")?;
	let height = crop_bottom
		.checked_mul(2)
		.and_then(|crop| coded.1.checked_sub(crop))
		.context("invalid vertical H.265 crop")?;
	if width == 0 || height == 0 {
		bail!("visible size {width}x{height} is empty");
	}

	// What the level allows, or what the stream says it needs if that is more,
	// so a stream that understates its level still fits.
	let highest = usize::from(sps.max_sub_layers_minus1);
	let buffering = usize::from(sps.max_dec_pic_buffering_minus1[highest]) + 1;
	let max_dpb_frames = sps.max_dpb_size().max(buffering).min(MAX_DPB_SIZE);

	Ok(Format {
		codec: Codec::H265,
		// Main 10 and Main Still Picture streams that reach here hold 8-bit
		// samples, which the Main profile decodes.
		profile: VAProfile::VAProfileHEVCMain,
		coded,
		visible: (width, height),
		max_dpb_frames,
	})
}

/// Adds a slice's parameters and data to `picture`.
fn add_slice(
	picture: &mut Picture<crate::PictureNew, Handle>,
	context: &Rc<crate::Context>,
	param: SliceParameterBufferHEVC,
	data: Vec<u8>,
) -> anyhow::Result<()> {
	picture.add_buffer(create_buffer(
		context,
		BufferType::SliceParameter(SliceParameter::HEVC(param)),
	)?);
	picture.add_buffer(create_buffer(context, BufferType::SliceData(data))?);
	Ok(())
}

/// Describes a DPB picture to the driver, flagged with the part of the current
/// reference picture set it is in.
fn va_picture(entry: &DpbEntry<Handle>, rps: &RefPicSet) -> PictureHEVC {
	let pic = entry.0.borrow();
	let contains =
		|list: &[Option<DpbEntry<Handle>>]| list.iter().flatten().any(|other| Rc::ptr_eq(&other.0, &entry.0));

	let mut flags = 0;
	if matches!(pic.reference(), Reference::LongTerm) {
		flags |= VA_PICTURE_HEVC_LONG_TERM_REFERENCE;
	}
	if contains(&rps.st_curr_before) {
		flags |= VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE;
	} else if contains(&rps.st_curr_after) {
		flags |= VA_PICTURE_HEVC_RPS_ST_CURR_AFTER;
	} else if contains(&rps.lt_curr) {
		flags |= VA_PICTURE_HEVC_RPS_LT_CURR;
	}

	PictureHEVC::new(entry.1.surface.id(), pic.pic_order_cnt_val, flags)
}

fn build_pic_param(
	pic: &PictureData,
	surface_id: crate::VASurfaceID,
	references: &[DpbEntry<Handle>],
	rps: &RefPicSet,
	sps: &Sps,
	pps: &Pps,
) -> BufferType {
	let mut reference_frames = [PictureHEVC::new(VA_INVALID_ID, 0, VA_PICTURE_HEVC_INVALID); MAX_VA_REFERENCES];
	for (slot, entry) in reference_frames.iter_mut().zip(references) {
		*slot = va_picture(entry, rps);
	}

	let pic_fields = HevcPicFields::new(
		u32::from(sps.chroma_format_idc),
		u32::from(sps.separate_colour_plane_flag),
		u32::from(sps.pcm_enabled_flag),
		u32::from(sps.scaling_list_enabled_flag),
		u32::from(pps.transform_skip_enabled_flag),
		u32::from(sps.amp_enabled_flag),
		u32::from(sps.strong_intra_smoothing_enabled_flag),
		u32::from(pps.sign_data_hiding_enabled_flag),
		u32::from(pps.constrained_intra_pred_flag),
		u32::from(pps.cu_qp_delta_enabled_flag),
		u32::from(pps.weighted_pred_flag),
		u32::from(pps.weighted_bipred_flag),
		u32::from(pps.transquant_bypass_enabled_flag),
		u32::from(pps.tiles_enabled_flag),
		u32::from(pps.entropy_coding_sync_enabled_flag),
		u32::from(pps.loop_filter_across_slices_enabled_flag),
		u32::from(pps.loop_filter_across_tiles_enabled_flag),
		u32::from(sps.pcm_loop_filter_disabled_flag),
		// NoPicReorderingFlag and NoBiPredFlag are hints, which ffmpeg and
		// GStreamer leave unset too.
		0,
		0,
	);

	let rap_pic_flag = (NaluType::BlaWLp..=NaluType::CraNut).contains(&pic.nalu_type);
	let slice_parsing_fields = HevcSliceParsingFields::new(
		u32::from(pps.lists_modification_present_flag),
		u32::from(sps.long_term_ref_pics_present_flag),
		u32::from(sps.temporal_mvp_enabled_flag),
		u32::from(pps.cabac_init_present_flag),
		u32::from(pps.output_flag_present_flag),
		u32::from(pps.dependent_slice_segments_enabled_flag),
		u32::from(pps.slice_chroma_qp_offsets_present_flag),
		u32::from(sps.sample_adaptive_offset_enabled_flag),
		u32::from(pps.deblocking_filter_override_enabled_flag),
		u32::from(pps.deblocking_filter_disabled_flag),
		u32::from(pps.slice_segment_header_extension_present_flag),
		u32::from(rap_pic_flag),
		u32::from(pic.nalu_type.is_idr()),
		u32::from(pic.nalu_type.is_irap()),
	);

	let pic_param = PictureParameterBufferHEVC::new(
		va_current_picture(pic, surface_id),
		reference_frames,
		sps.pic_width_in_luma_samples,
		sps.pic_height_in_luma_samples,
		&pic_fields,
		sps.max_dec_pic_buffering_minus1[usize::from(sps.max_sub_layers_minus1)],
		sps.bit_depth_luma_minus8,
		sps.bit_depth_chroma_minus8,
		sps.pcm_sample_bit_depth_luma_minus1,
		sps.pcm_sample_bit_depth_chroma_minus1,
		sps.log2_min_luma_coding_block_size_minus3,
		sps.log2_diff_max_min_luma_coding_block_size,
		sps.log2_min_luma_transform_block_size_minus2,
		sps.log2_diff_max_min_luma_transform_block_size,
		sps.log2_min_pcm_luma_coding_block_size_minus3,
		sps.log2_diff_max_min_pcm_luma_coding_block_size,
		sps.max_transform_hierarchy_depth_intra,
		sps.max_transform_hierarchy_depth_inter,
		pps.init_qp_minus26,
		pps.diff_cu_qp_delta_depth,
		pps.cb_qp_offset,
		pps.cr_qp_offset,
		pps.log2_parallel_merge_level_minus2,
		pps.num_tile_columns_minus1,
		pps.num_tile_rows_minus1,
		// VA takes the signalled sizes only; the last of each is inferred.
		std::array::from_fn(|i| pps.column_width_minus1[i] as u16),
		std::array::from_fn(|i| pps.row_height_minus1[i] as u16),
		&slice_parsing_fields,
		sps.log2_max_pic_order_cnt_lsb_minus4,
		sps.num_short_term_ref_pic_sets,
		sps.num_long_term_ref_pics_sps,
		pps.num_ref_idx_l0_default_active_minus1,
		pps.num_ref_idx_l1_default_active_minus1,
		pps.beta_offset_div2,
		pps.tc_offset_div2,
		pps.num_extra_slice_header_bits,
		pic.short_term_ref_pic_set_size_bits,
	);

	BufferType::PictureParameter(PictureParameter::HEVC(pic_param))
}

/// Describes the picture being decoded, which is in no reference picture set
/// of its own.
fn va_current_picture(pic: &PictureData, surface_id: crate::VASurfaceID) -> PictureHEVC {
	PictureHEVC::new(surface_id, pic.pic_order_cnt_val, 0)
}

/// The scaling lists a picture is dequantized with, or `None` for the flat
/// default the driver assumes without an IQ matrix (7.3.2.2.1, 7.3.2.3.1).
fn scaling_lists<'a>(sps: &'a Sps, pps: &'a Pps) -> Option<&'a ScalingLists> {
	if !sps.scaling_list_enabled_flag {
		None
	} else if pps.scaling_list_data_present_flag || !sps.scaling_list_data_present_flag {
		// The PPS's lists, or the defaults the parser fills in when neither
		// parameter set sends any.
		Some(&pps.scaling_list)
	} else {
		Some(&sps.scaling_list)
	}
}

/// 6.5.3: the up-right diagonal scan of a `ROWS` by `ROWS` block, as raster
/// positions.
const fn up_right_diagonal<const N: usize, const ROWS: usize>() -> [usize; N] {
	let mut scan = [0; N];
	let mut i = 0;
	let mut x = 0i32;
	let mut y = 0i32;
	while i < N {
		while y >= 0 {
			if x < ROWS as i32 && y < ROWS as i32 {
				scan[i] = (x + ROWS as i32 * y) as usize;
				i += 1;
			}
			y -= 1;
			x += 1;
		}
		y = x;
		x = 0;
	}
	scan
}

const UP_RIGHT_DIAGONAL_4X4: [usize; 16] = up_right_diagonal::<16, 4>();
const UP_RIGHT_DIAGONAL_8X8: [usize; 64] = up_right_diagonal::<64, 8>();

/// Reorders a list coded in up-right diagonal scan into raster order, which is
/// how VA takes it.
fn raster<const N: usize>(list: &[u8; N], scan: &[usize; N]) -> [u8; N] {
	let mut out = [0; N];
	for (&value, &position) in list.iter().zip(scan) {
		out[position] = value;
	}
	out
}

fn build_iq_matrix(lists: &ScalingLists) -> BufferType {
	// VA carries the 32x32 lists for luma only: matrixId 0 (intra) and 3
	// (inter) of the six the syntax indexes.
	BufferType::IQMatrix(IQMatrix::HEVC(IQMatrixBufferHEVC::new(
		lists.scaling_list_4x4.map(|list| raster(&list, &UP_RIGHT_DIAGONAL_4X4)),
		lists.scaling_list_8x8.map(|list| raster(&list, &UP_RIGHT_DIAGONAL_8X8)),
		lists
			.scaling_list_16x16
			.map(|list| raster(&list, &UP_RIGHT_DIAGONAL_8X8)),
		[0, 3].map(|id| raster(&lists.scaling_list_32x32[id], &UP_RIGHT_DIAGONAL_8X8)),
		lists.scaling_list_dc_coef_minus8_16x16.map(|dc| (dc + 8) as u8),
		[0, 3].map(|id| (lists.scaling_list_dc_coef_minus8_32x32[id] + 8) as u8),
	)))
}

/// Maps a slice's reference list onto indices into the picture parameters'
/// reference frames, the way VA takes it.
fn va_ref_pic_list(list: &[Option<DpbEntry<Handle>>], references: &[DpbEntry<Handle>]) -> [u8; MAX_VA_REFERENCES] {
	let mut indices = [0xff; MAX_VA_REFERENCES];
	for (index, entry) in indices.iter_mut().zip(list) {
		if let Some(entry) = entry {
			if let Some(position) = references
				.iter()
				.position(|reference| Rc::ptr_eq(&reference.0, &entry.0))
			{
				*index = position as u8;
			}
		}
	}
	indices
}

fn build_slice_param(
	slice: &Slice,
	sps: &Sps,
	list0: &[Option<DpbEntry<Handle>>],
	list1: &[Option<DpbEntry<Handle>>],
	references: &[DpbEntry<Handle>],
) -> anyhow::Result<SliceParameterBufferHEVC> {
	let hdr = &slice.header;

	let long_slice_flags = HevcLongSliceFlags::new(
		// Set on the last slice by `finish_picture`, once it is known to be last.
		0,
		u32::from(hdr.dependent_slice_segment_flag),
		hdr.type_ as u32,
		u32::from(hdr.colour_plane_id),
		u32::from(hdr.sao_luma_flag),
		u32::from(hdr.sao_chroma_flag),
		u32::from(hdr.mvd_l1_zero_flag),
		u32::from(hdr.cabac_init_flag),
		u32::from(hdr.temporal_mvp_enabled_flag),
		u32::from(hdr.deblocking_filter_disabled_flag),
		u32::from(hdr.collocated_from_l0_flag),
		u32::from(hdr.loop_filter_across_slices_enabled_flag),
	);

	let collocated_ref_idx = if hdr.temporal_mvp_enabled_flag {
		hdr.collocated_ref_idx
	} else {
		0xff
	};

	// 7-56: VA takes the derived chroma offsets, not the deltas the slice codes.
	let pwt = &hdr.pred_weight_table;
	let half_range = sps.wp_offset_half_range_c as i32;
	let chroma_offset = |delta_weight: i8, delta_offset: i16| {
		let weight = (1 << pwt.chroma_log2_weight_denom) + i32::from(delta_weight);
		let offset = half_range + i32::from(delta_offset) - ((half_range * weight) >> pwt.chroma_log2_weight_denom);
		offset.clamp(-half_range, half_range - 1) as i8
	};
	let b = hdr.type_.is_b();
	let mut chroma_offset_l0 = [[0; 2]; MAX_VA_REFERENCES];
	let mut chroma_offset_l1 = [[0; 2]; MAX_VA_REFERENCES];
	for i in 0..MAX_VA_REFERENCES {
		for j in 0..2 {
			chroma_offset_l0[i][j] = chroma_offset(pwt.delta_chroma_weight_l0[i][j], pwt.delta_chroma_offset_l0[i][j]);
			if b {
				chroma_offset_l1[i][j] =
					chroma_offset(pwt.delta_chroma_weight_l1[i][j], pwt.delta_chroma_offset_l1[i][j]);
			}
		}
	}
	let (delta_luma_weight_l1, luma_offset_l1, delta_chroma_weight_l1) = if b {
		(pwt.delta_luma_weight_l1, pwt.luma_offset_l1, pwt.delta_chroma_weight_l1)
	} else {
		Default::default()
	};

	Ok(SliceParameterBufferHEVC::new(
		u32::try_from(slice.nalu.size).context("slice NAL unit too large")?,
		0,
		VA_SLICE_DATA_FLAG_ALL,
		hdr.header_bit_size / 8,
		hdr.segment_address,
		[va_ref_pic_list(list0, references), va_ref_pic_list(list1, references)],
		&long_slice_flags,
		collocated_ref_idx,
		hdr.num_ref_idx_l0_active_minus1,
		hdr.num_ref_idx_l1_active_minus1,
		hdr.qp_delta,
		hdr.cb_qp_offset,
		hdr.cr_qp_offset,
		hdr.beta_offset_div2,
		hdr.tc_offset_div2,
		pwt.luma_log2_weight_denom,
		pwt.delta_chroma_log2_weight_denom,
		pwt.delta_luma_weight_l0,
		pwt.luma_offset_l0,
		pwt.delta_chroma_weight_l0,
		chroma_offset_l0,
		delta_luma_weight_l1,
		luma_offset_l1,
		delta_chroma_weight_l1,
		chroma_offset_l1,
		hdr.five_minus_max_num_merge_cand,
		u16::try_from(hdr.num_entry_point_offsets).context("too many entry points for a VA slice")?,
		0,
		u16::try_from(hdr.n_emulation_prevention_bytes)
			.context("too many emulation prevention bytes for a VA slice")?,
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// An SPS this decoder takes: Main, 8-bit 4:2:0, 64x64.
	fn main_sps() -> Sps {
		let mut sps = Sps {
			chroma_format_idc: 1,
			pic_width_in_luma_samples: 64,
			pic_height_in_luma_samples: 64,
			..Default::default()
		};
		sps.profile_tier_level.general_profile_idc = 1;
		sps
	}

	#[test]
	fn a_field_coded_sequence_is_rejected() {
		assert!(format(&main_sps()).is_ok());

		let mut sps = main_sps();
		sps.vui_parameters_present_flag = true;
		sps.vui_parameters.field_seq_flag = true;
		let err = format(&sps).expect_err("a field-coded sequence decoded as frames");
		assert!(err.to_string().contains("interlaced"), "{err}");
	}

	/// C.5.2.2: a picture is overdue once it has waited SpsMaxLatencyPictures,
	/// the reorder limit plus sps_max_latency_increase_plus1 minus one, not
	/// sps_max_latency_increase_plus1 alone.
	#[test]
	fn the_latency_limit_counts_from_the_reorder_limit() {
		let mut sps = main_sps();
		sps.max_num_reorder_pics[0] = 2;
		sps.max_latency_increase_plus1[0] = 1;

		let mut dpb = Dpb::<()>::default();
		dpb.set_max_num_pics(MAX_DPB_SIZE);
		let store = |dpb: &mut Dpb<()>, poc| {
			let mut pic = PictureData::default();
			pic.pic_output_flag = true;
			pic.pic_order_cnt_val = poc;
			dpb.store_picture(Rc::new(RefCell::new(pic)), ()).unwrap();
		};

		// Two waiting, the oldest for one picture: within both limits.
		store(&mut dpb, 0);
		store(&mut dpb, 4);
		assert!(!dpb.needs_additional_bumping(&sps));

		// Output the others around POC 4 until it has waited two pictures, which
		// is SpsMaxLatencyPictures, with still only two waiting.
		dpb.bump(false);
		store(&mut dpb, 2);
		assert!(!dpb.needs_additional_bumping(&sps));
		dpb.bump(false);
		store(&mut dpb, 6);
		assert!(dpb.needs_additional_bumping(&sps));
	}

	#[test]
	fn the_diagonal_scan_matches_the_specification() {
		// 6.5.3, worked by hand for a 4x4 block.
		assert_eq!(
			UP_RIGHT_DIAGONAL_4X4,
			[0, 4, 1, 8, 5, 2, 12, 9, 6, 3, 13, 10, 7, 14, 11, 15]
		);
		let mut sorted = UP_RIGHT_DIAGONAL_8X8;
		sorted.sort_unstable();
		assert_eq!(
			sorted,
			std::array::from_fn(|i| i),
			"the 8x8 scan visits each position once"
		);
	}
}
