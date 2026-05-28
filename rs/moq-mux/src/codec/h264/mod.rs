//! H.264 / AVC.
//!
//! Parses SPS NAL units and AVCDecoderConfigurationRecord blobs into
//! catalog-ready fields. The [`Avc1`] transmuxer rewrites Annex-B input
//! (inline SPS/PPS) as length-prefixed NALU + out-of-band avcC, which is
//! what every CMAF and MKV consumer expects. [`Import`] is the importer;
//! it auto-detects either wire shape from the leading bytes.

mod import;

pub use import::*;

use bytes::{Buf, BufMut, Bytes, BytesMut};

// H.264 NAL unit types (ISO/IEC 14496-10 §7.4.1).
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

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

	#[error("SPS too large for avcC length field ({0} > {max})", max = u16::MAX)]
	SpsTooLarge(usize),

	#[error("PPS too large for avcC length field ({0} > {max})", max = u16::MAX)]
	PpsTooLarge(usize),

	#[error("NAL too large for 4-byte length prefix")]
	NalTooLarge,

	#[error("NAL unit is too short")]
	NalTooShort,

	#[error("forbidden zero bit is not zero")]
	ForbiddenZeroBit,

	#[error("not initialized; call initialize() or with_mode() first")]
	NotInitialized,

	#[error("avc3 track not created")]
	Avc3TrackNotCreated,

	#[error("missing timestamp")]
	MissingTimestamp,

	#[error("decode_stream is avc3 only")]
	StreamNotAvc3,

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

/// Parsed AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2).
///
/// Just the codec-config fields that the hang catalog records. The original
/// avcC bytes are still what gets stored as the catalog `description`; this
/// struct is for the field extraction.
#[derive(Debug, Clone)]
pub struct Avcc {
	pub profile: u8,
	pub constraints: u8,
	pub level: u8,
	/// NALU length size in bytes (typically 4).
	pub length_size: usize,
	/// Resolution from the embedded SPS, if one was present and parseable.
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
}

impl Avcc {
	/// Parse an AVCDecoderConfigurationRecord buffer.
	pub fn parse(avcc: &[u8]) -> Result<Self> {
		if avcc.len() < 6 {
			return Err(Error::AvccTooShort);
		}

		let profile = avcc[1];
		let constraints = avcc[2];
		let level = avcc[3];
		let length_size = (avcc[4] & 0x03) as usize + 1;
		let num_sps = avcc[5] & 0x1f;

		let (mut coded_width, mut coded_height) = (None, None);
		if num_sps > 0 && avcc.len() >= 8 {
			let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
			let sps_start = 8;
			let sps_end = sps_start + sps_len;
			if sps_end <= avcc.len()
				&& sps_len > 1
				&& let Ok(sps) = Sps::parse(&avcc[sps_start..sps_end])
			{
				coded_width = Some(sps.coded_width);
				coded_height = Some(sps.coded_height);
			}
		}

		Ok(Self {
			profile,
			constraints,
			level,
			length_size,
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

/// Build an AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2) from a
/// single SPS and PPS NAL.
pub(crate) fn build_avcc(sps_nal: &[u8], pps_nal: &[u8]) -> Result<Bytes> {
	if sps_nal.len() > u16::MAX as usize {
		return Err(Error::SpsTooLarge(sps_nal.len()));
	}
	if pps_nal.len() > u16::MAX as usize {
		return Err(Error::PpsTooLarge(pps_nal.len()));
	}
	if sps_nal.len() < 4 {
		return Err(Error::SpsTooShort);
	}

	let profile_idc = sps_nal[1];
	let constraints = sps_nal[2];
	let level_idc = sps_nal[3];

	let mut out = BytesMut::with_capacity(11 + sps_nal.len() + pps_nal.len());
	out.put_u8(1); // configurationVersion
	out.put_u8(profile_idc);
	out.put_u8(constraints);
	out.put_u8(level_idc);
	out.put_u8(0xff); // reserved (6 bits) | lengthSizeMinusOne (2 bits = 3)
	out.put_u8(0xe1); // reserved (3 bits) | numOfSequenceParameterSets (5 bits = 1)
	out.put_u16(sps_nal.len() as u16);
	out.put_slice(sps_nal);
	out.put_u8(1); // numOfPictureParameterSets
	out.put_u16(pps_nal.len() as u16);
	out.put_slice(pps_nal);
	Ok(out.freeze())
}

/// Transform H.264 frames from Annex-B (inline SPS/PPS, "avc3") to
/// length-prefixed NALU (out-of-band AVCDecoderConfigurationRecord, "avc1").
///
/// The avcC is synthesized from cached SPS+PPS the first time both are
/// observed and is exposed via [`Self::avcc`]. Once [`Self::avcc`] returns
/// `Some`, all subsequent calls to [`Self::transform`] return length-prefixed
/// sample data suitable for an avc1 container (e.g. MKV `V_MPEG4/ISO/AVC` with
/// the avcC in CodecPrivate).
pub struct Avc1 {
	avcc: Option<Bytes>,
	sps: Option<Bytes>,
	pps: Option<Bytes>,
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
			sps: None,
			pps: None,
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
		// Parse Annex-B NALs, strip SPS/PPS into the cache, length-prefix
		// the rest. NalIterator advances the Bytes cursor; the trailing NAL
		// has to be pulled separately via flush().
		let mut buf = payload.clone();
		let mut nal_iter = crate::codec::annexb::NalIterator::new(&mut buf);

		let mut out = BytesMut::with_capacity(payload.remaining());
		let mut sps_pps_changed = false;
		let mut emitted_any_slice = false;

		loop {
			let nal = match nal_iter.next() {
				Some(Ok(n)) => n,
				Some(Err(e)) => return Err(e.into()),
				None => break,
			};
			if self.process_nal(&nal, &mut out, &mut sps_pps_changed)? {
				emitted_any_slice = true;
			}
		}

		if let Some(nal) = nal_iter.flush()? {
			let was_slice = self.process_nal(&nal, &mut out, &mut sps_pps_changed)?;
			if was_slice {
				emitted_any_slice = true;
			}
		}

		if sps_pps_changed {
			self.rebuild_avcc()?;
		}

		if !emitted_any_slice {
			return Ok(None);
		}

		Ok(Some(out.freeze()))
	}

	/// Process one NAL: SPS/PPS go into the cache, everything else gets
	/// length-prefixed and appended to `out`. Returns true if the NAL was a
	/// slice (i.e. produced sample bytes).
	fn process_nal(&mut self, nal: &Bytes, out: &mut BytesMut, sps_pps_changed: &mut bool) -> Result<bool> {
		if nal.is_empty() {
			return Ok(false);
		}
		let nal_type = nal[0] & 0x1f;
		match nal_type {
			NAL_TYPE_SPS => {
				if self.sps.as_deref() != Some(nal.as_ref()) {
					self.sps = Some(nal.clone());
					*sps_pps_changed = true;
				}
				Ok(false)
			}
			NAL_TYPE_PPS => {
				if self.pps.as_deref() != Some(nal.as_ref()) {
					self.pps = Some(nal.clone());
					*sps_pps_changed = true;
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

	fn rebuild_avcc(&mut self) -> Result<()> {
		let (Some(sps), Some(pps)) = (&self.sps, &self.pps) else {
			return Ok(());
		};
		self.avcc = Some(build_avcc(sps, pps)?);
		Ok(())
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
