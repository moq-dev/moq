//! H.264 / AVC.
//!
//! Parses SPS NAL units and AVCDecoderConfigurationRecord blobs into
//! catalog-ready fields. The [`Avc1`] transmuxer rewrites Annex-B input
//! (inline SPS/PPS) as length-prefixed NALU + out-of-band avcC, which is
//! what every CMAF and MKV consumer expects. [`Import`] is the importer;
//! it auto-detects either wire shape from the leading bytes.

mod import;

pub use import::*;

use anyhow::Context;
use bytes::{Buf, BufMut, Bytes, BytesMut};

// H.264 NAL unit types (ISO/IEC 14496-10 §7.4.1).
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

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
	pub fn parse(nal: &[u8]) -> anyhow::Result<Self> {
		anyhow::ensure!(nal.len() >= 4, "SPS NAL too short");
		let rbsp = h264_parser::nal::ebsp_to_rbsp(&nal[1..]);
		let sps = h264_parser::Sps::parse(&rbsp).context("failed to parse SPS")?;
		Ok(Self {
			profile: sps.profile_idc,
			constraints: pack_constraint_flags(&sps),
			level: sps.level_idc,
			coded_width: sps.width,
			coded_height: sps.height,
		})
	}
}

/// Parsed AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2).
///
/// Just the codec-config fields that the hang catalog records. The original
/// avcC bytes are still what gets stored as the catalog `description`; this
/// struct is for the field extraction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Avcc {
	pub profile: u8,
	pub constraints: u8,
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
	pub fn parse(avcc: &[u8]) -> anyhow::Result<Self> {
		anyhow::ensure!(avcc.len() >= 7, "AVCDecoderConfigurationRecord too short");

		let profile = avcc[1];
		let constraints = avcc[2];
		let level = avcc[3];
		let length_size = (avcc[4] & 0x03) as usize + 1;
		let num_sps = (avcc[5] & 0x1f) as usize;

		let mut sps = Vec::with_capacity(num_sps);
		let mut pos = read_param_set_array(avcc, 6, num_sps, &mut sps)?;

		anyhow::ensure!(avcc.len() > pos, "AVCDecoderConfigurationRecord truncated");
		let num_pps = avcc[pos] as usize;
		pos += 1;
		let mut pps = Vec::with_capacity(num_pps);
		read_param_set_array(avcc, pos, num_pps, &mut pps)?;

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
pub(crate) fn build_avcc(sps_nals: &[Bytes], pps_nals: &[Bytes]) -> anyhow::Result<Bytes> {
	let first_sps = sps_nals.first().context("avcC requires at least one SPS")?;
	anyhow::ensure!(first_sps.len() >= 4, "SPS NAL too short");
	// numOfSequenceParameterSets is a 5-bit field, numOfPictureParameterSets a byte.
	anyhow::ensure!(
		sps_nals.len() <= 0x1f,
		"too many SPS for avcC ({} > 31)",
		sps_nals.len()
	);
	anyhow::ensure!(
		pps_nals.len() <= u8::MAX as usize,
		"too many PPS for avcC ({} > 255)",
		pps_nals.len()
	);
	for (label, nal) in sps_nals
		.iter()
		.map(|n| ("SPS", n))
		.chain(pps_nals.iter().map(|n| ("PPS", n)))
	{
		anyhow::ensure!(
			nal.len() <= u16::MAX as usize,
			"{label} too large for avcC length field ({} > {})",
			nal.len(),
			u16::MAX
		);
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
	pub fn transform(&mut self, payload: Bytes) -> anyhow::Result<Option<Bytes>> {
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
				Some(Err(e)) => return Err(e),
				None => break,
			};
			if process_nal(&nal, &mut out, &mut frame_sps, &mut frame_pps)? {
				emitted_any_slice = true;
			}
		}

		if let Some(nal) = nal_iter.flush()? {
			if process_nal(&nal, &mut out, &mut frame_sps, &mut frame_pps)? {
				emitted_any_slice = true;
			}
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

	fn rebuild_avcc(&mut self) -> anyhow::Result<()> {
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
) -> anyhow::Result<bool> {
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
			let len = u32::try_from(nal.len()).context("NAL too large for 4-byte length prefix")?;
			out.extend_from_slice(&len.to_be_bytes());
			out.extend_from_slice(nal);
			Ok(true)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const SC4: &[u8] = &[0, 0, 0, 1];

	fn annexb_frame(nals: &[&[u8]]) -> Bytes {
		let mut buf = BytesMut::new();
		for nal in nals {
			buf.extend_from_slice(SC4);
			buf.extend_from_slice(nal);
		}
		buf.freeze()
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
	fn avcc_parse_separates_sps_and_pps() {
		let sps = Bytes::from_static(&[0x67, 0x42, 0xc0, 0x1f, 0xde]);
		let pps0 = Bytes::from_static(&[0x68, 0xce, 0x3c, 0x80]);
		let pps1 = Bytes::from_static(&[0x68, 0xce, 0x3c, 0x81]);

		let avcc = build_avcc(std::slice::from_ref(&sps), &[pps0.clone(), pps1.clone()]).unwrap();
		let parsed = Avcc::parse(&avcc).unwrap();

		assert_eq!(parsed.length_size, 4);
		assert_eq!(parsed.sps, vec![sps]);
		assert_eq!(parsed.pps, vec![pps0, pps1]);
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
