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
	data: &[u8],
	length_size: usize,
	pts: moq_net::Timestamp,
) -> crate::Result<crate::container::Frame> {
	Ok(crate::container::Frame {
		timestamp: pts,
		payload: data.to_vec().into(),
		keyframe: avc1_is_keyframe(data, length_size),
		duration: None,
	})
}

/// Detect whether an avc1-shaped (length-prefixed) buffer contains an IDR slice.
fn avc1_is_keyframe(data: &[u8], length_size: usize) -> bool {
	let mut offset = 0;
	while offset + length_size <= data.len() {
		let nal_len = match length_size {
			1 => data[offset] as usize,
			2 => u16::from_be_bytes([data[offset], data[offset + 1]]) as usize,
			3 => u32::from_be_bytes([0, data[offset], data[offset + 1], data[offset + 2]]) as usize,
			4 => u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize,
			_ => return false,
		};
		offset += length_size;
		if offset + nal_len > data.len() {
			break;
		}
		if nal_len > 0 && data[offset] & 0x1f == 5 {
			return true; // IDR slice
		}
		offset += nal_len;
	}
	false
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

/// SPS and PPS NAL units extracted from an avcC.
#[derive(Debug, Clone)]
pub struct AvccParamSets {
	/// NALU length size in bytes (typically 4).
	pub length_size: usize,
	pub sps: Vec<Bytes>,
	pub pps: Vec<Bytes>,
}

/// Pull the SPS and PPS NAL units out of an AVCDecoderConfigurationRecord.
pub fn parse_avcc_param_sets(avcc: &[u8]) -> Result<AvccParamSets> {
	if avcc.len() < 7 {
		return Err(Error::AvccTooShort);
	}
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

	Ok(AvccParamSets { length_size, sps, pps })
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
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];

		let avcc = build_avcc(sps, pps).unwrap();
		let (length_size, params) = avcc_params(&avcc).unwrap();

		assert_eq!(length_size, 4);
		assert_eq!(params.len(), 2);
		assert_eq!(params[0].as_ref(), sps);
		assert_eq!(params[1].as_ref(), pps);
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
