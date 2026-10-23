//! POC is a timestamp only when the stream declares a proportional clock.
//!
//! h264-reader owns AVC syntax; scuffle-h265 owns HEVC SPS/VUI syntax. The
//! private HEVC reader stops at the first independent slice's POC prefix.
//! Only owned POC state survives between calls.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use h264_reader::nal::slice::{
	DecRefPicMarking, Field, FieldPic, MemoryManagementControlOperation, PicOrderCountLsb, SliceHeader,
};
use h264_reader::nal::sps::PicOrderCntType;
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::nal::{pps::PicParameterSet, sps::SeqParameterSet};
use moq_net::Timestamp;

use super::Unit;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Rate {
	numerator: u64,
	denominator: u32,
}

#[derive(Clone, Copy)]
struct Anchor {
	poc: i64,
	pts: Timestamp,
	rate: Rate,
}

#[derive(Default)]
pub(crate) struct Clock {
	pub temporal_poc: bool,
	avc: AvcPoc,
	hevc: Option<HevcPoc>,
	anchor: Option<Anchor>,
	avc_ctx: Option<Arc<AvcContext>>,
	avc_update: Option<Option<Arc<AvcContext>>>,
	hevc_ctx: Option<Arc<HevcContext>>,
	hevc_update: Option<Option<Arc<HevcContext>>>,
}

impl Clock {
	pub fn reset(&mut self) {
		self.avc = AvcPoc::default();
		self.hevc = None;
		self.anchor = None;
		self.avc_update = None;
		self.hevc_update = None;
	}

	pub fn commit(&mut self) {
		if let Some(ctx) = self.avc_update.take() {
			self.avc_ctx = ctx;
		}
		if let Some(ctx) = self.hevc_update.take() {
			self.hevc_ctx = ctx;
		}
	}

	fn stamp(
		&mut self,
		explicit: Option<Timestamp>,
		poc: i64,
		rate: Option<Rate>,
		reset: bool,
	) -> anyhow::Result<Timestamp> {
		if reset {
			self.anchor = None;
		}
		let Some(rate) = rate else {
			self.anchor = None;
			return explicit.ok_or_else(|| {
				anyhow::anyhow!("TS video access unit has no timestamp or signalled proportional POC clock")
			});
		};
		if let Some(pts) = explicit {
			self.anchor = Some(Anchor { poc, pts, rate });
			return Ok(pts);
		}
		let anchor = self
			.anchor
			.filter(|anchor| anchor.rate == rate)
			.ok_or_else(|| anyhow::anyhow!("TS video access unit has no timestamp anchor for its POC clock"))?;
		// Always measure from the explicit anchor, retaining fractional ticks rather
		// than accumulating the rounding error of a frame-by-frame increment.
		let scale = i128::from(anchor.pts.scale().as_u64());
		let denominator = scale
			.checked_mul(rate.denominator.into())
			.ok_or_else(|| anyhow::anyhow!("TS video timestamp overflow"))?;
		let delta = (i128::from(poc) - i128::from(anchor.poc))
			.checked_mul(i128::from(rate.numerator))
			.and_then(|n| n.checked_mul(scale));
		let micros = i128::from(anchor.pts.value())
			.checked_mul(rate.denominator.into())
			.and_then(|n| n.checked_add(delta?))
			.and_then(|n| n.checked_mul(1_000_000))
			.and_then(|n| u64::try_from(n / denominator).ok())
			.ok_or_else(|| anyhow::anyhow!("TS video timestamp outside the media clock"))?;
		Ok(Timestamp::from_micros(micros)?)
	}

	pub fn avc<'a>(&mut self, unit: &Unit, params: impl Iterator<Item = &'a Bytes>) -> anyhow::Result<Timestamp> {
		let parsed = (|| {
			let mut ctx = self.avc_ctx.clone().unwrap_or_default();
			let bootstrap: Vec<_> = if self.avc_ctx.is_none() {
				params.cloned().collect()
			} else {
				Vec::new()
			};
			for bytes in bootstrap.into_iter().chain(nals(&unit.data)?) {
				let nal = RefNal::new(&bytes, &[], true);
				let header = nal.header().map_err(|err| format!("{err:?}"))?;
				match header.nal_unit_type() {
					UnitType::SeqParameterSet => Arc::make_mut(&mut ctx).0.put_seq_param_set(
						SeqParameterSet::from_bits(nal.rbsp_bits()).map_err(|err| format!("{err:?}"))?,
					),
					UnitType::PicParameterSet => {
						let pps =
							PicParameterSet::from_bits(&ctx.0, nal.rbsp_bits()).map_err(|err| format!("{err:?}"))?;
						Arc::make_mut(&mut ctx).0.put_pic_param_set(pps);
					}
					UnitType::SliceLayerWithoutPartitioningNonIdr
					| UnitType::SliceLayerWithoutPartitioningIdr
					| UnitType::SliceDataPartitionALayer => {
						let (hdr, sps, _) = SliceHeader::from_bits(&ctx.0, &mut nal.rbsp_bits(), header, None)
							.map_err(|err| format!("{err:?}"))?;
						let info = sps.vui_parameters.as_ref().and_then(|vui| vui.timing_info.as_ref());
						let rate = info
							.filter(|info| {
								self.temporal_poc
									&& info.fixed_frame_rate_flag
									&& info.num_units_in_tick != 0
									&& info.time_scale != 0
							})
							.map(|info| Rate {
								numerator: info.num_units_in_tick.into(),
								denominator: info.time_scale,
							});
						let mut picture = AvcSlice {
							idr: hdr.idr_pic_id.is_some(),
							reference: header.nal_ref_idc() != 0,
							..Default::default()
						};
						picture.header.frame_num = hdr.frame_num;
						picture.header.field_pic_flag = hdr.field_pic != FieldPic::Frame;
						picture.header.bottom_field_flag = hdr.field_pic == FieldPic::Field(Field::Bottom);
						match hdr.pic_order_cnt_lsb {
							Some(PicOrderCountLsb::Frame(lsb)) => picture.header.pic_order_cnt_lsb = lsb,
							Some(PicOrderCountLsb::FieldsAbsolute {
								pic_order_cnt_lsb,
								delta_pic_order_cnt_bottom,
							}) => {
								picture.header.pic_order_cnt_lsb = pic_order_cnt_lsb;
								picture.header.delta_pic_order_cnt_bottom = delta_pic_order_cnt_bottom;
							}
							Some(PicOrderCountLsb::FieldsDelta(delta)) => picture.header.delta_pic_order_cnt = delta,
							None => {}
						}
						picture.header.mmco5 = matches!(hdr.dec_ref_pic_marking, Some(DecRefPicMarking::Adaptive(ref ops)) if ops.iter().any(|op| matches!(op, MemoryManagementControlOperation::AllRefPicturesUnused)));
						let sps = AvcSps::from(sps);
						let (poc, mmco5) = self.avc.compute(&picture, &sps)?;
						let idr = picture.idr;
						self.avc_update = Some(Some(ctx));
						return Ok((poc, rate, idr, mmco5));
					}
					_ => {}
				}
			}
			Err("missing AVC picture".to_owned())
		})();
		let (poc, rate, reset, mmco5) = match parsed {
			Ok(parsed) => parsed,
			Err(err) => {
				self.reset();
				self.avc_update = Some(None);
				return unit
					.pts
					.ok_or_else(|| anyhow::anyhow!("TS video timestamp cannot be derived: {err}"));
			}
		};
		let pts = self.stamp(unit.pts, poc, rate, reset)?;
		// MMCO 5 renumbers the current picture to zero for subsequent POC
		// derivation. Its presentation time is still the one just calculated.
		if mmco5 {
			self.anchor = rate.map(|rate| Anchor { poc: 0, pts, rate });
		}
		Ok(pts)
	}

	pub fn hevc<'a>(&mut self, unit: &Unit, params: impl Iterator<Item = &'a Bytes>) -> anyhow::Result<Timestamp> {
		let parsed = (|| {
			let mut ctx = self.hevc_ctx.clone().unwrap_or_default();
			let bootstrap: Vec<_> = if self.hevc_ctx.is_none() {
				params.cloned().collect()
			} else {
				Vec::new()
			};
			for bytes in bootstrap.into_iter().chain(nals(&unit.data)?) {
				let kind = (bytes[0] >> 1) & 63;
				match kind {
					33 => {
						let sps = scuffle_h265::SpsNALUnit::parse(&bytes[..])
							.map_err(|err| err.to_string())?
							.rbsp;
						Arc::make_mut(&mut ctx)
							.sets
							.insert(sps.sps_seq_parameter_set_id, Arc::new(sps));
					}
					34 => {
						let rbsp = h264_parser::nal::ebsp_to_rbsp(bytes.get(2..).ok_or("short HEVC PPS")?);
						let pps = HevcPps::parse(&rbsp)?;
						Arc::make_mut(&mut ctx).pictures.insert(pps.id, pps);
					}
					0..=31 => {
						let second = *bytes.get(1).ok_or("short HEVC NAL header")?;
						if bytes[0] & 1 != 0 || second & 0xf8 != 0 {
							return Err("multilayer HEVC timing is unsupported".to_owned());
						}
						let rbsp = h264_parser::nal::ebsp_to_rbsp(bytes.get(2..).ok_or("short HEVC slice")?);
						let (lsb, width, rate) = hevc_prefix(&rbsp, kind, &ctx.pictures, &ctx.sets)?;
						let idr = matches!(kind, 19 | 20);
						let reset = idr || matches!(kind, 16..=18) || (kind == 21 && self.hevc.is_none());
						let max = 1i64 << width;
						let prev = self.hevc.unwrap_or_default();
						let msb = if reset {
							Some(0)
						} else if lsb < prev.lsb && prev.lsb - lsb >= max / 2 {
							prev.msb.checked_add(max)
						} else if lsb > prev.lsb && lsb - prev.lsb > max / 2 {
							prev.msb.checked_sub(max)
						} else {
							Some(prev.msb)
						}
						.ok_or("HEVC POC overflow")?;
						let poc = msb.checked_add(lsb).ok_or("HEVC POC overflow")?;
						if second & 7 == 1 && !matches!(kind, 0 | 2 | 4 | 6..=9) {
							self.hevc = Some(HevcPoc { msb, lsb });
						}
						self.hevc_update = Some(Some(ctx));
						return Ok((poc, rate, reset));
					}
					_ => {}
				}
			}
			Err("missing HEVC picture".to_owned())
		})();
		match parsed {
			Ok((poc, rate, reset)) => self.stamp(unit.pts, poc, rate, reset),
			Err(err) => {
				self.reset();
				self.hevc_update = Some(None);
				unit.pts
					.ok_or_else(|| anyhow::anyhow!("TS video timestamp cannot be derived: {err}"))
			}
		}
	}
}

#[derive(Clone)]
struct HevcPps {
	id: u32,
	sps: u32,
	output: bool,
	extra: u8,
}

impl HevcPps {
	fn parse(rbsp: &[u8]) -> Result<Self, String> {
		let parsed = (|| {
			let mut r = h264_parser::bitreader::BitReader::new(rbsp);
			let id = h264_parser::eg::read_ue(&mut r)?;
			let sps = h264_parser::eg::read_ue(&mut r)?;
			r.skip_bits(1)?; // dependent_slice_segments_enabled_flag, absent on the first slice
			let output = r.read_flag()?;
			let extra = r.read_bits(3)? as u8;
			h264_parser::Result::Ok(Self { id, sps, output, extra })
		})();
		let pps = parsed.map_err(|err| err.to_string())?;
		if pps.id > 63 || pps.sps > 15 {
			return Err("invalid HEVC parameter-set id".to_owned());
		}
		Ok(pps)
	}
}

fn hevc_prefix(
	rbsp: &[u8],
	kind: u8,
	pictures: &BTreeMap<u32, HevcPps>,
	sets: &BTreeMap<u64, Arc<scuffle_h265::SpsRbsp>>,
) -> Result<(i64, u32, Option<Rate>), String> {
	let mut r = h264_parser::bitreader::BitReader::new(rbsp);
	if !r.read_flag().map_err(|err| err.to_string())? {
		return Err("missing first independent HEVC slice".to_owned());
	}
	if (16..=23).contains(&kind) {
		r.skip_bits(1).map_err(|err| err.to_string())?;
	}
	let id = h264_parser::eg::read_ue(&mut r).map_err(|err| err.to_string())?;
	let pps = pictures.get(&id).ok_or("missing HEVC PPS")?;
	let sps = sets.get(&u64::from(pps.sps)).ok_or("missing HEVC SPS")?;
	// The first independent slice has no segment address or inherited header.
	// Stop as soon as its POC is available, before reference lists and slice data.
	r.skip_bits(pps.extra.into()).map_err(|err| err.to_string())?;
	let slice_type = h264_parser::eg::read_ue(&mut r).map_err(|err| err.to_string())?;
	if slice_type > 2 {
		return Err("invalid HEVC slice type".to_owned());
	}
	if pps.output {
		r.skip_bits(1).map_err(|err| err.to_string())?;
	}
	if sps.separate_colour_plane_flag {
		r.skip_bits(2).map_err(|err| err.to_string())?;
	}
	let width = u32::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4;
	let lsb = if matches!(kind, 19 | 20) {
		0
	} else {
		i64::from(r.read_bits(width).map_err(|err| err.to_string())?)
	};
	let info = sps.vui_parameters.as_ref().and_then(|vui| vui.vui_timing_info.as_ref());
	let rate = info
		.filter(|info| info.poc_proportional_to_timing_flag)
		.map(|info| Rate {
			numerator: u64::from(info.num_units_in_tick.get())
				* (u64::from(info.num_ticks_poc_diff_one_minus1.unwrap_or(0)) + 1),
			denominator: info.time_scale.get(),
		});
	Ok((lsb, width, rate))
}

fn nals(data: &[u8]) -> Result<Vec<Bytes>, String> {
	let mut data = data;
	let mut nals = Vec::new();
	let mut iter = crate::codec::annexb::NalIterator::new(&mut data);
	for nal in iter.by_ref() {
		nals.push(nal.map_err(|err| err.to_string())?);
	}
	if let Some(nal) = iter.flush().map_err(|err| err.to_string())? {
		nals.push(nal);
	}
	Ok(nals)
}

#[derive(Default)]
struct AvcPoc {
	frame_num: i64,
	frame_offset: i64,
	ref_msb: i64,
	ref_lsb: i64,
	ref_top: i64,
	ref_bottom_field: bool,
	ref_mmco5: bool,
	mmco5: bool,
}

impl AvcPoc {
	fn compute(&mut self, slice: &AvcSlice, sps: &AvcSps) -> Result<(i64, bool), String> {
		let hdr = &slice.header;
		let idr = slice.idr;
		let reference = slice.reference;
		let frame = i64::from(hdr.frame_num);
		let offset = if idr || self.mmco5 {
			0
		} else if frame < self.frame_num {
			self.frame_offset
				.checked_add(1 << (sps.log2_max_frame_num_minus4 + 4))
				.ok_or("AVC frame number overflow")?
		} else {
			self.frame_offset
		};
		let (top, bottom, msb) = match sps.pic_order_cnt_type {
			0 => {
				let (prev_msb, prev_lsb) = if idr {
					(0, 0)
				} else if self.ref_mmco5 {
					(0, if self.ref_bottom_field { 0 } else { self.ref_top })
				} else {
					(self.ref_msb, self.ref_lsb)
				};
				let lsb = i64::from(hdr.pic_order_cnt_lsb);
				let max = 1 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
				let msb = if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
					prev_msb.checked_add(max)
				} else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
					prev_msb.checked_sub(max)
				} else {
					Some(prev_msb)
				}
				.ok_or("AVC POC overflow")?;
				let top = msb.checked_add(lsb).ok_or("AVC POC overflow")?;
				let bottom = top
					.checked_add(if hdr.field_pic_flag {
						0
					} else {
						i64::from(hdr.delta_pic_order_cnt_bottom)
					})
					.ok_or("AVC POC overflow")?;
				(top, bottom, msb)
			}
			1 | 2 => {
				let mut abs = offset.checked_add(frame).ok_or("AVC POC overflow")?;
				let base = if sps.pic_order_cnt_type == 2 {
					if idr {
						0
					} else {
						abs.checked_mul(2)
							.and_then(|n| n.checked_sub(i64::from(!reference)))
							.ok_or("AVC POC overflow")?
					}
				} else {
					let cycle = i64::from(sps.num_ref_frames_in_pic_order_cnt_cycle);
					if cycle == 0 {
						abs = 0;
					}
					if !reference && abs > 0 {
						abs -= 1;
					}
					let mut expected = 0i64;
					if abs > 0 {
						let offsets = &sps.offset_for_ref_frame[..cycle as usize];
						let sum: i64 = offsets.iter().map(|&n| i64::from(n)).sum();
						expected = ((abs - 1) / cycle).checked_mul(sum).ok_or("AVC POC overflow")?;
						for &n in &offsets[..=((abs - 1) % cycle) as usize] {
							expected = expected.checked_add(n.into()).ok_or("AVC POC overflow")?;
						}
					}
					if !reference {
						expected = expected
							.checked_add(sps.offset_for_non_ref_pic.into())
							.ok_or("AVC POC overflow")?;
					}
					expected
				};
				let top = base
					.checked_add(i64::from(hdr.delta_pic_order_cnt[0]))
					.ok_or("AVC POC overflow")?;
				let bottom = if sps.pic_order_cnt_type == 2 {
					Some(top)
				} else if hdr.field_pic_flag {
					top.checked_add(sps.offset_for_top_to_bottom_field.into())
				} else {
					top.checked_add(sps.offset_for_top_to_bottom_field.into())
						.and_then(|n| n.checked_add(hdr.delta_pic_order_cnt[1].into()))
				}
				.ok_or("AVC POC overflow")?;
				(top, bottom, 0)
			}
			_ => return Err("invalid AVC POC type".to_owned()),
		};
		let poc = if hdr.field_pic_flag {
			if hdr.bottom_field_flag { bottom } else { top }
		} else {
			top.min(bottom)
		};
		let mmco5 = hdr.mmco5;
		self.frame_num = if mmco5 { 0 } else { frame };
		self.frame_offset = if mmco5 { 0 } else { offset };
		self.mmco5 = mmco5;
		if reference {
			self.ref_msb = msb;
			self.ref_lsb = hdr.pic_order_cnt_lsb.into();
			self.ref_top = top - if mmco5 { poc } else { 0 };
			self.ref_bottom_field = hdr.field_pic_flag && hdr.bottom_field_flag;
			self.ref_mmco5 = mmco5;
		}
		Ok((poc, mmco5))
	}
}

#[derive(Clone, Copy, Default)]
struct HevcPoc {
	msb: i64,
	lsb: i64,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn slice(frame: u16, lsb: u16, reference: bool, idr: bool) -> AvcSlice {
		AvcSlice {
			header: AvcHeader {
				frame_num: frame,
				pic_order_cnt_lsb: lsb.into(),
				..Default::default()
			},
			idr,
			reference,
		}
	}

	#[test]
	fn sparse_reordered_pocs_and_wrap_use_the_previous_reference() {
		let mut state = AvcPoc::default();
		let sps = AvcSps {
			pic_order_cnt_type: 0,
			log2_max_pic_order_cnt_lsb_minus4: 0,
			..Default::default()
		};
		for (frame, lsb, reference, expected) in [
			(0, 0, true, 0),
			(1, 6, true, 6),
			(2, 2, false, 2),
			(2, 7, false, 7),
			(2, 14, true, 14),
			(3, 2, true, 18),
			(4, 15, false, 15),
			(4, 4, true, 20),
		] {
			assert_eq!(
				state
					.compute(&slice(frame, lsb, reference, frame == 0), &sps)
					.unwrap()
					.0,
				expected
			);
		}
	}

	#[test]
	fn poc_types_one_and_two_keep_cycle_offsets_and_nonreference_pictures() {
		let mut sps = AvcSps {
			pic_order_cnt_type: 1,
			num_ref_frames_in_pic_order_cnt_cycle: 2,
			offset_for_non_ref_pic: -1,
			..Default::default()
		};
		sps.offset_for_ref_frame = vec![2, 4];
		let mut state = AvcPoc::default();
		for (frame, reference, expected) in [(0, true, 0), (1, true, 2), (2, true, 6), (3, false, 5), (3, true, 8)] {
			assert_eq!(
				state.compute(&slice(frame, 0, reference, frame == 0), &sps).unwrap().0,
				expected
			);
		}
		sps.pic_order_cnt_type = 2;
		let mut state = AvcPoc::default();
		for (frame, reference, expected) in [
			(0, true, 0),
			(15, true, 30),
			(0, false, 31),
			(0, true, 32),
			(1, true, 34),
		] {
			assert_eq!(
				state
					.compute(&slice(frame, 0, reference, frame == 0 && expected == 0), &sps)
					.unwrap()
					.0,
				expected
			);
		}
	}

	#[test]
	fn mmco_five_renumbers_future_pictures_without_reusing_the_old_clock() {
		let sps = AvcSps {
			pic_order_cnt_type: 0,
			log2_max_pic_order_cnt_lsb_minus4: 0,
			..Default::default()
		};
		let mut state = AvcPoc::default();
		state.compute(&slice(0, 0, true, true), &sps).unwrap();
		let mut reset = slice(1, 6, true, false);
		reset.header.mmco5 = true;
		assert_eq!(state.compute(&reset, &sps).unwrap(), (6, true));
		assert_eq!(state.compute(&slice(1, 2, true, false), &sps).unwrap().0, 2);
	}

	#[test]
	fn inferred_clock_rounds_only_after_combining_anchor_and_signed_poc_distance() {
		let mut clock = Clock::default();
		let rate = Some(Rate {
			numerator: 1001,
			denominator: 60_000,
		});
		let anchor = Timestamp::from_scale(123_123, 90_000).unwrap();
		clock.stamp(Some(anchor), 6, rate, true).unwrap();
		for poc in [2, 7, 12, 60_006] {
			let got = clock.stamp(None, poc, rate, false).unwrap();
			let expected = (123_123i128 * 2 + (i128::from(poc) - 6) * 3003) * 1_000_000 / 180_000;
			assert_eq!(got.as_micros(), expected as u128);
		}
		assert!(
			clock.stamp(None, 0, rate, true).is_err(),
			"a new POC epoch needs its own anchor"
		);
		assert!(
			clock.stamp(None, 1, None, false).is_err(),
			"a missing clock never repeats an earlier PTS"
		);
	}
	fn bits(input: &str) -> Vec<u8> {
		input
			.as_bytes()
			.chunks(8)
			.map(|chunk| {
				chunk
					.iter()
					.enumerate()
					.fold(0, |byte, (n, &bit)| byte | u8::from(bit == b'1') << (7 - n))
			})
			.collect()
	}

	#[test]
	fn hevc_poc_prefix_consumes_only_declared_fields_and_refuses_truncation() {
		for irap in [false, true] {
			for output in [false, true] {
				for separate in [false, true] {
					for extra in [0, 7] {
						let mut sps = scuffle_h265::SpsNALUnit::parse(crate::codec::h265::fixtures::SPS)
							.unwrap()
							.rbsp;
						sps.separate_colour_plane_flag = separate;
						sps.log2_max_pic_order_cnt_lsb_minus4 = 12;
						let sets = BTreeMap::from([(0, Arc::new(sps))]);
						let pictures = BTreeMap::from([(
							0,
							HevcPps {
								id: 0,
								sps: 0,
								output,
								extra,
							},
						)]);
						let head = format!(
							"1{}1{}011{}{}",
							if irap { "0" } else { "" },
							"0".repeat(extra as usize),
							if output { "1" } else { "" },
							if separate { "10" } else { "" }
						);
						let data = bits(&format!("{head}1010101010101010"));
						assert_eq!(
							hevc_prefix(&data, if irap { 21 } else { 1 }, &pictures, &sets)
								.unwrap()
								.0,
							0xaaaa
						);
						for len in 0..data.len() {
							assert!(hevc_prefix(&data[..len], if irap { 21 } else { 1 }, &pictures, &sets).is_err());
						}
						// IDR still consumes the IRAP/output/colour-plane fields,
						// but has no POC LSB in the slice header.
						if irap {
							assert_eq!(hevc_prefix(&bits(&head), 19, &pictures, &sets).unwrap().0, 0);
						}
					}
				}
			}
		}
	}

	#[test]
	fn hevc_missing_parameter_sets_dependent_slices_and_unbounded_ue_are_refused() {
		let pictures = BTreeMap::new();
		let sets = BTreeMap::new();
		for input in [vec![], vec![0; 64], bits("0"), bits("11")] {
			assert!(hevc_prefix(&input, 1, &pictures, &sets).is_err());
		}
		for input in [vec![], vec![0; 64], bits("00000010000001")] {
			assert!(HevcPps::parse(&input).is_err());
		}
	}
}

#[derive(Default)]
struct AvcSlice {
	header: AvcHeader,
	idr: bool,
	reference: bool,
}
#[derive(Default)]
struct AvcHeader {
	frame_num: u16,
	pic_order_cnt_lsb: u32,
	field_pic_flag: bool,
	bottom_field_flag: bool,
	delta_pic_order_cnt_bottom: i32,
	delta_pic_order_cnt: [i32; 2],
	mmco5: bool,
}
#[derive(Default)]
struct AvcSps {
	pic_order_cnt_type: u8,
	log2_max_frame_num_minus4: u8,
	log2_max_pic_order_cnt_lsb_minus4: u8,
	offset_for_non_ref_pic: i32,
	offset_for_top_to_bottom_field: i32,
	num_ref_frames_in_pic_order_cnt_cycle: u8,
	offset_for_ref_frame: Vec<i32>,
}
impl From<&SeqParameterSet> for AvcSps {
	fn from(sps: &SeqParameterSet) -> Self {
		let mut out = Self {
			log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
			..Default::default()
		};
		match &sps.pic_order_cnt {
			PicOrderCntType::TypeZero {
				log2_max_pic_order_cnt_lsb_minus4,
			} => out.log2_max_pic_order_cnt_lsb_minus4 = *log2_max_pic_order_cnt_lsb_minus4,
			PicOrderCntType::TypeOne {
				offset_for_non_ref_pic,
				offset_for_top_to_bottom_field,
				offsets_for_ref_frame,
				..
			} => {
				out.pic_order_cnt_type = 1;
				out.offset_for_non_ref_pic = *offset_for_non_ref_pic;
				out.offset_for_top_to_bottom_field = *offset_for_top_to_bottom_field;
				out.num_ref_frames_in_pic_order_cnt_cycle = offsets_for_ref_frame.len() as u8;
				out.offset_for_ref_frame = offsets_for_ref_frame.clone();
			}
			PicOrderCntType::TypeTwo => out.pic_order_cnt_type = 2,
		}
		out
	}
}

#[derive(Default)]
struct AvcContext(h264_reader::Context);
impl Clone for AvcContext {
	fn clone(&self) -> Self {
		let mut ctx = h264_reader::Context::new();
		for sps in self.0.sps() {
			ctx.put_seq_param_set(sps.clone());
		}
		for pps in self.0.pps() {
			ctx.put_pic_param_set(pps.clone());
		}
		Self(ctx)
	}
}

#[derive(Clone, Default)]
struct HevcContext {
	sets: BTreeMap<u64, Arc<scuffle_h265::SpsRbsp>>,
	pictures: BTreeMap<u32, HevcPps>,
}
