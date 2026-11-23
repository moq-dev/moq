//! Telling openh264 that a stream comes out in decoding order.
//!
//! openh264 decides how to output pictures by `profile_idc` alone
//! (`ReorderPicturesInDisplay` in `welsDecoderExt.cpp`). Baseline and Scalable
//! Baseline pictures come out as soon as they are decoded. Every other profile
//! goes through a reordering buffer that, short of B slices, releases a picture
//! only once the next one has been decoded. So a Main or High stream without B
//! slices comes out one access unit late, a full frame interval, even when its
//! SPS says outright that output order is decoding order: picture order count
//! type 2 (8.2.1.3), or a VUI `max_num_reorder_frames` of 0 (E.2.1). openh264
//! parses both and acts on neither. The VA-API, NVENC and VideoToolbox encoders
//! code Main or High, so this delays their streams at every openh264 viewer.
//!
//! [`relabel`] marks such an SPS Scalable Baseline (`profile_idc` 83) on its way
//! into openh264. openh264 parses that exactly as High (`ParseSps` in
//! `au_parser.cpp` reads the chroma format, bit depths and scaling matrices for
//! both), so a High SPS changes in its `profile_idc` byte alone. A Main SPS
//! also gains those High syntax elements, at the values Main infers for them:
//! 4:2:0, 8 bits, no transform bypass, flat scaling. Relabelling Main as Baseline would be
//! shorter, but openh264 modifies the reference lists of a CABAC stream another
//! way once it is labelled Baseline (`InitRefPicList` in `decoder_core.cpp`).
//!
//! The label has one other effect. Any SPS labelled Scalable Baseline clears
//! the decoder's `bAvcBasedFlag` for good (`ParseSps`), which turns on the
//! checks openh264 keeps for SVC streams: every slice of an access unit must
//! share its picture order count, and the picture width of the sequence
//! (`CheckIntegrityNalUnitsList` in `decoder_core.cpp`,
//! `WelsTargetSliceConstruction` in `decode_slice.cpp`). A single-layer frame
//! stream meets both, across a resolution change too, since the width is taken
//! afresh at each new sequence.
//!
//! An SPS that allows reordering, or that this cannot read, passes unchanged.

use bytes::Bytes;
use h264_reader::nal::sps::{PicOrderCntType, SeqParameterSet};
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::rbsp::BitRead;
use moq_mux::codec::annexb;

/// `nal_unit_type` of a sequence parameter set.
const SPS: u8 = 7;
/// `nal_unit_type` of a picture parameter set.
const PPS: u8 = 8;
const BASELINE: u8 = 66;
const MAIN: u8 = 77;
const HIGH: u8 = 100;
/// The profile openh264 parses as High and outputs as Baseline.
const SCALABLE_BASELINE: u8 = 83;

/// What [`Sequences::relabel`] made of an access unit.
pub(super) struct Relabelled {
	/// The access unit with every SPS that rules out reordering relabelled, or
	/// `None` when no SPS in it needs that.
	pub access_unit: Option<Bytes>,
	/// Whether openh264 outputs the access unit's picture as soon as it
	/// decodes, by the SPS its first slice uses, or `None` when that is unknown.
	pub immediate: Option<bool>,
}

/// The parameter sets seen so far, by ID: how openh264 outputs each SPS, and
/// which SPS each PPS uses. A slice names its PPS, so this finds the SPS that
/// sets its output order even when an access unit carries several.
pub(super) struct Sequences {
	sps: [Option<bool>; 32],
	pps: [Option<u8>; 256],
}

impl Default for Sequences {
	fn default() -> Self {
		Self {
			sps: [None; 32],
			pps: [None; 256],
		}
	}
}

impl Sequences {
	/// Relabels every SPS in `access_unit` that rules out reordering so openh264
	/// outputs each picture as soon as it is decoded, and reports how openh264
	/// outputs the access unit's picture.
	///
	/// Parameter sets come ahead of the first slice of an access unit (7.4.1.2.3),
	/// so the scan stops at that slice: an access unit without an SPS costs a look
	/// at the headers of the NAL units ahead of it and the start of that slice's,
	/// and only one with an SPS to relabel is copied.
	pub(super) fn relabel(&mut self, access_unit: &Bytes) -> Relabelled {
		let data = access_unit.as_ref();
		let mut relabelled = Vec::new();
		let mut immediate = None;
		let Some((offset, size)) = annexb::find_start_code(data) else {
			return Relabelled {
				access_unit: None,
				immediate,
			};
		};
		let mut start = offset + size;
		while let Some(&header) = data.get(start) {
			let next = annexb::find_start_code(&data[start..]);
			let end = next.map_or(data.len(), |(offset, _)| start + offset);
			let nal = &data[start..end];
			match header & 0x1f {
				// The first slice: its PPS names the SPS that sets the output order.
				1 | 5 => {
					immediate = slice_pps(nal)
						.and_then(|pps| self.pps[usize::from(pps)])
						.and_then(|sps| self.sps[usize::from(sps)]);
					break;
				}
				// Data partitions, and the slice extensions of SVC (20) and MVC (21).
				2..=4 | 20 | 21 => break,
				SPS => {
					let output = match sps(nal) {
						Some(sps) => {
							relabelled.push((start..end, sps));
							true
						}
						// openh264 outputs by `profile_idc`, the first payload byte.
						None => matches!(nal.get(1), Some(&(BASELINE | SCALABLE_BASELINE))),
					};
					if let Some(id) = sps_id(nal) {
						self.sps[usize::from(id)] = Some(output);
					}
				}
				PPS => {
					if let Some((pps, sps)) = pps_ids(nal) {
						self.pps[usize::from(pps)] = Some(sps);
					}
				}
				_ => {}
			}
			let Some((offset, size)) = next else { break };
			start += offset + size;
		}
		if relabelled.is_empty() {
			return Relabelled {
				access_unit: None,
				immediate,
			};
		}

		let grown: usize = relabelled
			.iter()
			.map(|(at, sps)| sps.len().saturating_sub(at.len()))
			.sum();
		let mut out = Vec::with_capacity(data.len() + grown);
		let mut copied = 0;
		for (at, sps) in relabelled {
			out.extend_from_slice(&data[copied..at.start]);
			out.extend_from_slice(&sps);
			copied = at.end;
		}
		out.extend_from_slice(&data[copied..]);
		Relabelled {
			access_unit: Some(out.into()),
			immediate,
		}
	}
}

/// The `seq_parameter_set_id` of an SPS NAL unit, after its profile,
/// constraint flags, and level.
fn sps_id(nal: &[u8]) -> Option<u8> {
	let mut bits = RefNal::new(nal, &[], true).rbsp_bits();
	for _ in 0..24 {
		bits.read_bit("profile, constraints, level").ok()?;
	}
	u8::try_from(bits.read_ue("seq_parameter_set_id").ok()?)
		.ok()
		.filter(|&id| id < 32)
}

/// A PPS NAL unit's own ID and the ID of the SPS it uses.
fn pps_ids(nal: &[u8]) -> Option<(u8, u8)> {
	let mut bits = RefNal::new(nal, &[], true).rbsp_bits();
	let pps = u8::try_from(bits.read_ue("pic_parameter_set_id").ok()?).ok()?;
	let sps = u8::try_from(bits.read_ue("seq_parameter_set_id").ok()?)
		.ok()
		.filter(|&id| id < 32)?;
	Some((pps, sps))
}

/// The `pic_parameter_set_id` of a slice NAL unit.
fn slice_pps(nal: &[u8]) -> Option<u8> {
	let mut bits = RefNal::new(nal, &[], true).rbsp_bits();
	bits.read_ue("first_mb_in_slice").ok()?;
	bits.read_ue("slice_type").ok()?;
	u8::try_from(bits.read_ue("pic_parameter_set_id").ok()?).ok()
}

/// Returns the NAL unit `nal` relabelled, when it is an SPS of a Main or High
/// stream whose pictures come out in decoding order.
fn sps(nal: &[u8]) -> Option<Vec<u8>> {
	let parsed = RefNal::new(nal, &[], true);
	if parsed.header().ok()?.nal_unit_type() != UnitType::SeqParameterSet {
		return None;
	}
	let sps = SeqParameterSet::from_bits(parsed.rbsp_bits()).ok()?;
	let reorder = sps
		.vui_parameters
		.as_ref()
		.and_then(|vui| vui.bitstream_restrictions.as_ref())
		.map(|restrictions| restrictions.max_num_reorder_frames);
	if !matches!(sps.pic_order_cnt, PicOrderCntType::TypeTwo) && reorder != Some(0) {
		return None;
	}

	match u8::from(sps.profile_idc) {
		// The profile is the first payload byte, which is never an emulation
		// prevention byte, so the rest stays as it is.
		HIGH => {
			let mut nal = nal.to_vec();
			nal[1] = SCALABLE_BASELINE;
			Some(nal)
		}
		MAIN => {
			let (&header, payload) = nal.split_first()?;
			let mut rbsp = unescape(payload);
			// Zero bytes after the stop bit are `trailing_zero_8bits`.
			while rbsp.last() == Some(&0) {
				rbsp.pop();
			}
			let rbsp = main_as_high(&rbsp)?;
			let mut out = Vec::with_capacity(rbsp.len() + 4);
			out.push(header);
			escape(&rbsp, &mut out);
			Some(out)
		}
		_ => None,
	}
}

/// Rewrites a Main SPS's RBSP in High syntax, as Scalable Baseline.
///
/// The High-only elements go right after `seq_parameter_set_id` (7.3.2.1.1),
/// at the values Main infers for them (7.4.2.1.1).
fn main_as_high(rbsp: &[u8]) -> Option<Vec<u8>> {
	let bit = |at: usize| rbsp[at / 8] >> (7 - at % 8) & 1;
	let len = rbsp.len() * 8;
	// `seq_parameter_set_id`, ue(v), follows the profile, constraint flags and level.
	let mut at = 24;
	let mut zeros = 0;
	while at < len && bit(at) == 0 {
		zeros += 1;
		at += 1;
	}
	let id_end = at + 1 + zeros;
	// The stop bit, the last bit set.
	let last = rbsp.iter().rposition(|&byte| byte != 0)?;
	let stop = last * 8 + 7 - rbsp[last].trailing_zeros() as usize;
	if id_end > stop {
		return None;
	}

	let mut out = BitWriter::default();
	for at in 0..id_end {
		out.push(bit(at));
	}
	// chroma_format_idc = 1 (ue 010), bit_depth_luma_minus8 = 0 (ue 1),
	// bit_depth_chroma_minus8 = 0 (ue 1), qpprime_y_zero_transform_bypass_flag
	// = 0, seq_scaling_matrix_present_flag = 0.
	for high in [0, 1, 0, 1, 1, 0, 0] {
		out.push(high);
	}
	for at in id_end..stop {
		out.push(bit(at));
	}
	// rbsp_trailing_bits: the stop bit, then zeros to the byte.
	out.push(1);
	let mut bytes = out.finish();
	bytes[0] = SCALABLE_BASELINE;
	Some(bytes)
}

/// Writes bits most significant first, padding the last byte with zeros.
#[derive(Default)]
struct BitWriter {
	bytes: Vec<u8>,
	len: usize,
}

impl BitWriter {
	fn push(&mut self, bit: u8) {
		if self.len.is_multiple_of(8) {
			self.bytes.push(0);
		}
		if bit != 0 {
			*self.bytes.last_mut().expect("pushed above") |= 0x80 >> (self.len % 8);
		}
		self.len += 1;
	}

	fn finish(self) -> Vec<u8> {
		self.bytes
	}
}

/// Removes the emulation prevention bytes from a NAL unit's payload (7.4.1).
fn unescape(payload: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(payload.len());
	let mut zeros = 0;
	for &byte in payload {
		if zeros >= 2 && byte == 3 {
			zeros = 0;
			continue;
		}
		zeros = if byte == 0 { zeros + 1 } else { 0 };
		out.push(byte);
	}
	out
}

/// Appends `rbsp` to `out` with emulation prevention bytes, so no start code
/// prefix appears inside the NAL unit (7.4.1).
fn escape(rbsp: &[u8], out: &mut Vec<u8>) {
	let mut zeros = 0;
	for &byte in rbsp {
		if zeros >= 2 && byte <= 3 {
			out.push(3);
			zeros = 0;
		}
		zeros = if byte == 0 { zeros + 1 } else { 0 };
		out.push(byte);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The SPS moq-vaapi's encoder writes for 640x360 at 20fps: Main, picture
	/// order count type 0, and a VUI that rules out reordering.
	const VAAPI_MAIN: &[u8] = &[
		0x67, 0x4d, 0x00, 0x28, 0x88, 0x89, 0x40, 0x50, 0x17, 0xfc, 0xb8, 0x0b, 0x50, 0x60, 0x10, 0x64, 0x00, 0x00,
		0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xa0, 0x36, 0x82, 0x21, 0x1a, 0x80,
	];

	/// libx264's Main SPS with two B frames: picture order count type 0 and a
	/// VUI that allows two frames of reordering.
	const X264_MAIN_BFRAMES: &[u8] = &[
		0x67, 0x4d, 0x40, 0x0a, 0xec, 0xa2, 0x13, 0x60, 0x22, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00,
		0x78, 0x1e, 0x24, 0x4b, 0x2c,
	];

	fn parse(nal: &[u8]) -> SeqParameterSet {
		SeqParameterSet::from_bits(RefNal::new(nal, &[], true).rbsp_bits()).expect("a valid SPS")
	}

	/// Escaping undoes unescaping, including at a run of zeros.
	#[test]
	fn emulation_prevention_round_trips() {
		let payload = [
			0x4d, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x03,
		];
		let rbsp = unescape(&payload);
		assert_eq!(rbsp, [0x4d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x03]);
		let mut escaped = Vec::new();
		escape(&rbsp, &mut escaped);
		assert_eq!(escaped, payload);
	}

	/// A Main SPS comes back as Scalable Baseline in High syntax, and says
	/// everything else it said before.
	#[test]
	fn a_main_sps_gains_the_high_syntax() {
		let before = parse(VAAPI_MAIN);
		assert_eq!(u8::from(before.profile_idc), MAIN);
		let after = parse(&sps(VAAPI_MAIN).expect("relabelled"));
		assert_eq!(u8::from(after.profile_idc), SCALABLE_BASELINE);
		assert_eq!(
			format!("{after:?}"),
			format!("{before:?}").replacen("ProfileIdc(77)", "ProfileIdc(83)", 1)
		);
	}

	/// A High SPS changes in its profile byte alone.
	#[test]
	fn a_high_sps_changes_its_profile_alone() {
		let relabelled = sps(VAAPI_MAIN).expect("relabelled");
		// The relabelled Main SPS is a High one but for its profile.
		let mut high = relabelled.to_vec();
		high[1] = HIGH;
		assert_eq!(sps(&high), Some(relabelled));
	}

	/// An SPS that allows reordering, or whose pictures openh264 outputs as they
	/// decode already, passes unchanged, and each reports how openh264 outputs it.
	#[test]
	fn other_sps_pass_unchanged() {
		assert_eq!(sps(X264_MAIN_BFRAMES), None);
		let bframes = Sequences::default().relabel(&Bytes::from_static(include_bytes!(
			"../../test_data/bframes_64x64_pattern_30f.h264"
		)));
		assert_eq!(bframes.access_unit, None, "High with two frames of reordering");
		assert_eq!(bframes.immediate, Some(false), "High with two frames of reordering");
		let baseline = Sequences::default().relabel(&Bytes::from_static(include_bytes!(
			"../../test_data/seq_64x64_pattern_5f.h264"
		)));
		assert_eq!(baseline.access_unit, None, "Constrained Baseline");
		assert_eq!(baseline.immediate, Some(true), "Constrained Baseline");
	}

	/// The relabelled access unit keeps every other NAL unit as it was, and an
	/// access unit without an SPS passes untouched.
	#[test]
	fn the_rest_of_the_access_unit_is_kept() {
		use annexb::NalIterator;

		let nals = |unit: &Bytes| {
			let mut buf = unit.clone();
			let mut iter = NalIterator::new(&mut buf);
			let mut nals: Vec<Bytes> = iter.by_ref().map(Result::unwrap).collect();
			nals.extend(iter.flush().unwrap());
			nals
		};
		let units = crate::decode::conformance::split(include_bytes!("../../test_data/main_64x64_yellow_5f.h264"));
		let mut sequences = Sequences::default();
		for unit in units {
			let before = nals(&unit.payload);
			let Relabelled {
				access_unit: Some(relabelled),
				immediate,
			} = sequences.relabel(&unit.payload)
			else {
				assert!(
					before.iter().all(|nal| nal[0] & 0x1f != SPS),
					"an SPS was left as it was"
				);
				continue;
			};
			assert_eq!(immediate, Some(true), "a relabelled SPS outputs immediately");
			let after = nals(&relabelled);
			assert_eq!(before.len(), after.len());
			for (before, after) in before.iter().zip(&after) {
				match before[0] & 0x1f {
					SPS => assert_eq!(u8::from(parse(after).profile_idc), SCALABLE_BASELINE),
					_ => assert_eq!(before, after),
				}
			}
		}
	}

	/// An access unit can carry several SPS, as one whose decoder configuration
	/// lists them all: the output order is the one of the SPS its slice uses,
	/// through its PPS, and stays known for the access units after it.
	#[test]
	fn the_output_order_follows_the_slice_s_sps() {
		// High (reorders) as SPS 0, and Baseline (outputs immediately) as SPS 1.
		let high = [0x67, HIGH, 0, 30, 0b1000_0000];
		let baseline = [0x67, BASELINE, 0, 30, 0b0100_0000];
		// PPS 0 uses SPS 0, PPS 1 uses SPS 1.
		let pps0 = [0x68, 0b1100_0000];
		let pps1 = [0x68, 0b0100_1000];
		// IDR slices: first_mb_in_slice 0, slice_type 2, then the PPS.
		let slice0 = [0x65, 0b1011_1000];
		let slice1 = [0x65, 0b1011_0100];
		let unit = |nals: &[&[u8]]| {
			let mut out = Vec::new();
			for nal in nals {
				out.extend_from_slice(&[0, 0, 0, 1]);
				out.extend_from_slice(nal);
			}
			Bytes::from(out)
		};

		let mut sequences = Sequences::default();
		let first = sequences.relabel(&unit(&[&high, &baseline, &pps0, &pps1, &slice0]));
		assert_eq!(first.immediate, Some(false), "the slice uses the High SPS");
		let switched = sequences.relabel(&unit(&[&high, &baseline, &pps0, &pps1, &slice1]));
		assert_eq!(switched.immediate, Some(true), "the slice uses the Baseline SPS");
		let later = sequences.relabel(&unit(&[&[0x41, 0b1011_1000]]));
		assert_eq!(later.immediate, Some(false), "a later picture uses PPS 0 again");
		assert_eq!(
			Sequences::default().relabel(&unit(&[&slice0])).immediate,
			None,
			"a slice whose parameter sets are unknown"
		);
	}
}
