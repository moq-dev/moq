//! H.265 / HEVC.
//!
//! The H.265 analogue of [`crate::codec::h264`]. Parses SPS NAL units
//! and HEVCDecoderConfigurationRecord blobs. The [`Hvc1`] transmuxer
//! rewrites Annex-B input (inline VPS/SPS/PPS) as length-prefixed NALU
//! + out-of-band hvcC. [`Export`] is the single-rendition Annex-B
//!   exporter; [`Import`] is the Annex-B importer.

mod export;
mod import;
mod split;

pub use export::*;
pub use import::*;
pub use split::*;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use scuffle_h265::{NALUnitType, SpsNALUnit};

/// H.265 parsing and transform errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("NAL unit is too short")]
	NalTooShort,

	#[error("{0} too large for hvcC length field ({1} > {max})", max = u16::MAX)]
	NalTooLargeForHvcc(&'static str, usize),

	#[error("NAL too large for 4-byte length prefix")]
	NalTooLarge,

	#[error("failed to parse SPS NAL unit")]
	SpsParse,

	#[error("missing level_idc in SPS")]
	MissingLevelIdc,

	#[error("forbidden zero bit is not zero")]
	ForbiddenZeroBit,

	#[error("not initialized")]
	NotInitialized,

	#[error("expected SPS before any frames")]
	MissingSps,

	#[error("missing timestamp")]
	MissingTimestamp,

	#[error("HEVCDecoderConfigurationRecord too short")]
	HvccTooShort,

	#[error("HEVCDecoderConfigurationRecord truncated")]
	HvccTruncated,

	#[error("hvc1 description for rendition {name:?} is missing VPS, SPS, or PPS (vps={vps}, sps={sps}, pps={pps})")]
	MissingParamSets {
		name: String,
		vps: usize,
		sps: usize,
		pps: usize,
	},

	#[error("annexb: {0}")]
	Annexb(#[from] crate::codec::annexb::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// VPS, SPS, and PPS NAL units extracted from an hvcC.
#[derive(Debug, Clone)]
pub struct HvccParamSets {
	/// NALU length size in bytes (typically 4).
	pub length_size: usize,
	pub vps: Vec<Bytes>,
	pub sps: Vec<Bytes>,
	pub pps: Vec<Bytes>,
}

/// Pull the VPS/SPS/PPS NAL units out of an HEVCDecoderConfigurationRecord.
pub fn parse_hvcc_param_sets(hvcc: &[u8]) -> Result<HvccParamSets> {
	if hvcc.len() < 23 {
		return Err(Error::HvccTooShort);
	}
	let length_size = (hvcc[21] & 0x3) as usize + 1;
	let num_arrays = hvcc[22] as usize;

	let mut vps = Vec::new();
	let mut sps = Vec::new();
	let mut pps = Vec::new();
	let mut pos: usize = 23;

	for _ in 0..num_arrays {
		let after_hdr = pos.checked_add(3).ok_or(Error::HvccTruncated)?;
		if hvcc.len() < after_hdr {
			return Err(Error::HvccTruncated);
		}
		let nal_type = hvcc[pos] & 0x3f;
		let num_nalus = u16::from_be_bytes([hvcc[pos + 1], hvcc[pos + 2]]) as usize;
		pos = after_hdr;

		for _ in 0..num_nalus {
			let after_len = pos.checked_add(2).ok_or(Error::HvccTruncated)?;
			if hvcc.len() < after_len {
				return Err(Error::HvccTruncated);
			}
			let len = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]) as usize;
			let after_nal = after_len.checked_add(len).ok_or(Error::HvccTruncated)?;
			if hvcc.len() < after_nal {
				return Err(Error::HvccTruncated);
			}
			let bytes = Bytes::copy_from_slice(&hvcc[after_len..after_nal]);
			pos = after_nal;

			match NALUnitType::from(nal_type) {
				NALUnitType::VpsNut => vps.push(bytes),
				NALUnitType::SpsNut => sps.push(bytes),
				NALUnitType::PpsNut => pps.push(bytes),
				_ => {}
			}
		}
	}

	Ok(HvccParamSets {
		length_size,
		vps,
		sps,
		pps,
	})
}

/// Annex-B → length-prefixed transmuxer; the H.265 analogue of
/// [`crate::codec::h264::Avc1`].
pub struct Hvc1 {
	hvcc: Option<Bytes>,
	vps: Option<Bytes>,
	sps: Option<Bytes>,
	pps: Option<Bytes>,
}

impl Default for Hvc1 {
	fn default() -> Self {
		Self::new()
	}
}

impl Hvc1 {
	/// Build a new transform for a hev1 source.
	pub fn new() -> Self {
		Self {
			hvcc: None,
			vps: None,
			sps: None,
			pps: None,
		}
	}

	/// The HEVCDecoderConfigurationRecord, available once VPS+SPS+PPS have been observed.
	pub fn hvcc(&self) -> Option<&Bytes> {
		self.hvcc.as_ref()
	}

	/// Convert one decoded frame's payload to the hvc1 wire shape.
	///
	/// Returns:
	/// - `Ok(Some(payload))` if a length-prefixed sample is ready to emit.
	/// - `Ok(None)` if the input contained only parameter sets and the
	///   transform is still waiting for slice NALs (hvcC may have been
	///   built as a side effect).
	pub fn transform(&mut self, payload: Bytes) -> Result<Option<Bytes>> {
		let mut buf = payload.clone();
		let mut nal_iter = crate::codec::annexb::NalIterator::new(&mut buf);

		let mut out = BytesMut::with_capacity(payload.remaining());
		let mut params_changed = false;
		let mut emitted_any_slice = false;

		loop {
			let nal = match nal_iter.next() {
				Some(Ok(n)) => n,
				Some(Err(e)) => return Err(e.into()),
				None => break,
			};
			if self.process_nal(&nal, &mut out, &mut params_changed)? {
				emitted_any_slice = true;
			}
		}

		if let Some(nal) = nal_iter.flush()? {
			let was_slice = self.process_nal(&nal, &mut out, &mut params_changed)?;
			if was_slice {
				emitted_any_slice = true;
			}
		}

		if params_changed {
			self.rebuild_hvcc()?;
		}

		if !emitted_any_slice {
			return Ok(None);
		}

		Ok(Some(out.freeze()))
	}

	fn process_nal(&mut self, nal: &Bytes, out: &mut BytesMut, params_changed: &mut bool) -> Result<bool> {
		if nal.is_empty() {
			return Ok(false);
		}
		// HEVC NAL header is 2 bytes; type is bits 1..=6 of byte 0.
		let nal_unit_type = (nal[0] >> 1) & 0x3f;
		let nal_type = NALUnitType::from(nal_unit_type);

		match nal_type {
			NALUnitType::VpsNut => {
				if self.vps.as_deref() != Some(nal.as_ref()) {
					self.vps = Some(nal.clone());
					*params_changed = true;
				}
				Ok(false)
			}
			NALUnitType::SpsNut => {
				if self.sps.as_deref() != Some(nal.as_ref()) {
					self.sps = Some(nal.clone());
					*params_changed = true;
				}
				Ok(false)
			}
			NALUnitType::PpsNut => {
				if self.pps.as_deref() != Some(nal.as_ref()) {
					self.pps = Some(nal.clone());
					*params_changed = true;
				}
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

	fn rebuild_hvcc(&mut self) -> Result<()> {
		let (Some(vps), Some(sps), Some(pps)) = (&self.vps, &self.sps, &self.pps) else {
			return Ok(());
		};
		self.hvcc = Some(build_hvcc(vps, sps, pps)?);
		Ok(())
	}
}

/// Build an HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3).
/// Single-layer streams only.
pub(crate) fn build_hvcc(vps_nal: &[u8], sps_nal: &[u8], pps_nal: &[u8]) -> Result<Bytes> {
	for (label, nal) in [("VPS", vps_nal), ("SPS", sps_nal), ("PPS", pps_nal)] {
		if nal.len() > u16::MAX as usize {
			return Err(Error::NalTooLargeForHvcc(label, nal.len()));
		}
	}

	let sps = SpsNALUnit::parse(&mut &sps_nal[..]).map_err(|_| Error::SpsParse)?;
	let profile = &sps.rbsp.profile_tier_level.general_profile;
	let level_idc = profile.level_idc.ok_or(Error::MissingLevelIdc)?;
	let constraint_flags = pack_constraint_flags(profile);
	let compat = profile.profile_compatibility_flag.bits().to_be_bytes();
	let num_temporal_layers = sps.rbsp.sps_max_sub_layers_minus1 + 1;

	let mut out = BytesMut::with_capacity(23 + vps_nal.len() + sps_nal.len() + pps_nal.len() + 9 * 3);
	out.put_u8(1); // configurationVersion
	out.put_u8(((profile.profile_space & 0x3) << 6) | ((profile.tier_flag as u8) << 5) | (profile.profile_idc & 0x1f));
	out.put_slice(&compat);
	out.put_slice(&constraint_flags);
	out.put_u8(level_idc);
	out.put_u16(0xf000); // min_spatial_segmentation_idc unknown
	out.put_u8(0xfc); // parallelismType mixed
	out.put_u8(0xfc | (sps.rbsp.chroma_format_idc & 0x3));
	out.put_u8(0xf8 | (sps.rbsp.bit_depth_luma_minus8 & 0x7));
	out.put_u8(0xf8 | (sps.rbsp.bit_depth_chroma_minus8 & 0x7));
	out.put_u16(0); // avgFrameRate unspecified
	out.put_u8(((num_temporal_layers & 0x7) << 3) | ((sps.rbsp.sps_temporal_id_nesting_flag as u8) << 2) | 0x3);
	out.put_u8(3); // numOfArrays

	for (nal_type, nal) in [
		(u8::from(NALUnitType::VpsNut), vps_nal),
		(u8::from(NALUnitType::SpsNut), sps_nal),
		(u8::from(NALUnitType::PpsNut), pps_nal),
	] {
		out.put_u8(0x80 | (nal_type & 0x3f)); // array_completeness = 1
		out.put_u16(1); // numNalus
		out.put_u16(nal.len() as u16);
		out.put_slice(nal);
	}

	Ok(out.freeze())
}

/// Extract the parameter-set NALs (VPS, SPS, PPS in array order) and the NALU
/// length size from an HEVCDecoderConfigurationRecord. The inverse of
/// [`build_hvcc`]; used to re-emit out-of-band hvc1 parameter sets as inline
/// Annex-B (e.g. for MPEG-TS).
pub(crate) fn hvcc_params(hvcc: &[u8]) -> anyhow::Result<(usize, Vec<Bytes>)> {
	anyhow::ensure!(hvcc.len() >= 23, "HEVCDecoderConfigurationRecord too short");
	let length_size = (hvcc[21] & 0x03) as usize + 1;
	let num_arrays = hvcc[22];

	let mut params = Vec::new();
	let mut pos = 23;
	for _ in 0..num_arrays {
		// Skip the array_completeness | NAL_unit_type byte.
		anyhow::ensure!(hvcc.len() >= pos + 3, "truncated hvcC NAL array header");
		pos += 1;
		let num_nalus = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]);
		pos += 2;
		for _ in 0..num_nalus {
			anyhow::ensure!(hvcc.len() >= pos + 2, "truncated hvcC NAL length");
			let len = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]) as usize;
			pos += 2;
			anyhow::ensure!(hvcc.len() >= pos + len, "hvcC NAL exceeds buffer");
			params.push(Bytes::copy_from_slice(&hvcc[pos..pos + len]));
			pos += len;
		}
	}

	Ok((length_size, params))
}

/// Pack the constraint flags from ITU H.265 V10 §7.3.3 Profile, tier and level syntax.
pub(crate) fn pack_constraint_flags(profile: &scuffle_h265::Profile) -> [u8; 6] {
	let mut flags = [0u8; 6];
	flags[0] = ((profile.progressive_source_flag as u8) << 7)
		| ((profile.interlaced_source_flag as u8) << 6)
		| ((profile.non_packed_constraint_flag as u8) << 5)
		| ((profile.frame_only_constraint_flag as u8) << 4);
	flags
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Hand-build an hvcC (the layout `build_hvcc` emits) and assert the
	/// parameter sets and length size are recovered. Built by hand rather than
	/// via `build_hvcc` so it doesn't need a real, parseable HEVC SPS.
	#[test]
	fn hvcc_params_parses_vps_sps_pps() {
		let vps = &[0x40, 0x01, 0x0c][..]; // NAL type 32
		let sps = &[0x42, 0x01, 0x01, 0x60][..]; // NAL type 33
		let pps = &[0x44, 0x01, 0xc0][..]; // NAL type 34

		let mut hvcc = BytesMut::new();
		hvcc.extend_from_slice(&[0u8; 21]); // fixed fields up to (but not including) byte 21
		hvcc.put_u8(0xfc | 0x03); // byte 21: ...| lengthSizeMinusOne = 3 -> length_size 4
		hvcc.put_u8(3); // numOfArrays
		for (nal_type, nal) in [
			(u8::from(NALUnitType::VpsNut), vps),
			(u8::from(NALUnitType::SpsNut), sps),
			(u8::from(NALUnitType::PpsNut), pps),
		] {
			hvcc.put_u8(0x80 | (nal_type & 0x3f));
			hvcc.put_u16(1); // numNalus
			hvcc.put_u16(nal.len() as u16);
			hvcc.put_slice(nal);
		}

		let (length_size, params) = hvcc_params(&hvcc).unwrap();
		assert_eq!(length_size, 4);
		assert_eq!(params.len(), 3);
		assert_eq!(params[0].as_ref(), vps);
		assert_eq!(params[1].as_ref(), sps);
		assert_eq!(params[2].as_ref(), pps);
	}
}
