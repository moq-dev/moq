use anyhow::Context;
use bytes::{Buf, BufMut, Bytes, BytesMut};

// H.264 NAL unit types (ISO/IEC 14496-10 §7.4.1).
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

/// Transform H.264 frames between Annex-B (inline SPS/PPS) and length-prefixed
/// NALU (out-of-band AVCDecoderConfigurationRecord, aka avc1).
///
/// Construct with [`Self::new`] passing the source's existing avcC if the
/// source was already avc1 (in which case [`Self::transform`] passes payloads
/// through unchanged). Pass `None` for Avc3 sources: avcC is synthesized from
/// cached SPS+PPS the first time both are observed and is exposed via
/// [`Self::avcc`].
///
/// Once [`Self::avcc`] returns `Some`, all subsequent calls to
/// [`Self::transform`] return length-prefixed sample data suitable for an
/// avc1 container (e.g. MKV `V_MPEG4/ISO/AVC` with the avcC in CodecPrivate).
pub struct Avc1 {
	/// Pre-supplied avcC (Avc1 source) or one we built ourselves (Avc3 source).
	avcc: Option<Bytes>,
	cached_sps: Option<Bytes>,
	cached_pps: Option<Bytes>,
	/// True iff the source is already length-prefixed (avc1). Set by [`Self::new`]
	/// when an avcC is supplied; for Avc3 sources we synthesize avcC and stay in
	/// "transform" mode.
	passthrough: bool,
}

impl Avc1 {
	/// Build a new transform. Pass the source's catalog `description` if it
	/// is already an `AVCDecoderConfigurationRecord` (avc1 source); pass
	/// `None` for an avc3 source whose samples are Annex-B with inline
	/// parameter sets.
	pub fn new(description: Option<Bytes>) -> Self {
		let passthrough = description.is_some();
		Self {
			avcc: description,
			cached_sps: None,
			cached_pps: None,
			passthrough,
		}
	}

	/// The AVCDecoderConfigurationRecord, available once SPS+PPS have been
	/// observed (or immediately if constructed with a pre-supplied avcC).
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
		if self.passthrough {
			// Avc1 source — pass through unchanged (cheap refcount clone).
			return Ok(Some(payload));
		}

		// Avc3 source — parse Annex-B NALs, strip SPS/PPS into the cache,
		// length-prefix the rest. NalIterator advances the Bytes cursor;
		// the trailing NAL has to be pulled separately via flush().
		let mut buf = payload.clone();
		let mut nal_iter = crate::import::annexb::NalIterator::new(&mut buf);

		let mut out = BytesMut::with_capacity(payload.remaining());
		let mut sps_pps_changed = false;
		let mut emitted_any_slice = false;

		loop {
			let nal = match nal_iter.next() {
				Some(Ok(n)) => n,
				Some(Err(e)) => return Err(e),
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
	fn process_nal(&mut self, nal: &Bytes, out: &mut BytesMut, sps_pps_changed: &mut bool) -> anyhow::Result<bool> {
		if nal.is_empty() {
			return Ok(false);
		}
		let nal_type = nal[0] & 0x1f;
		match nal_type {
			NAL_TYPE_SPS => {
				if self.cached_sps.as_deref() != Some(nal.as_ref()) {
					self.cached_sps = Some(nal.clone());
					*sps_pps_changed = true;
				}
				Ok(false)
			}
			NAL_TYPE_PPS => {
				if self.cached_pps.as_deref() != Some(nal.as_ref()) {
					self.cached_pps = Some(nal.clone());
					*sps_pps_changed = true;
				}
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

	fn rebuild_avcc(&mut self) -> anyhow::Result<()> {
		let (Some(sps), Some(pps)) = (&self.cached_sps, &self.cached_pps) else {
			return Ok(());
		};
		self.avcc = Some(build_avcc(sps, pps)?);
		Ok(())
	}
}

/// Build an AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1.2) from a
/// single SPS and PPS NAL.
fn build_avcc(sps_nal: &[u8], pps_nal: &[u8]) -> anyhow::Result<Bytes> {
	anyhow::ensure!(
		sps_nal.len() <= u16::MAX as usize,
		"SPS too large for avcC length field ({} > {})",
		sps_nal.len(),
		u16::MAX
	);
	anyhow::ensure!(
		pps_nal.len() <= u16::MAX as usize,
		"PPS too large for avcC length field ({} > {})",
		pps_nal.len(),
		u16::MAX
	);
	anyhow::ensure!(sps_nal.len() >= 4, "SPS NAL too short");

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

		let mut tx = Avc1::new(None);
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

		let mut tx = Avc1::new(None);
		let frame = annexb_frame(&[sps, pps]);
		assert!(tx.transform(frame).unwrap().is_none());
		assert!(tx.avcc().is_some());
	}

	#[test]
	fn avc1_passthrough() {
		let avcc = Bytes::from_static(&[1, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0, 0]);
		let mut tx = Avc1::new(Some(avcc.clone()));
		assert_eq!(tx.avcc(), Some(&avcc));

		let payload = Bytes::from_static(&[0, 0, 0, 4, 0x65, 0x88, 0x84, 0x21]);
		let out = tx.transform(payload.clone()).unwrap().unwrap();
		assert_eq!(out.as_ref(), payload.as_ref());
	}

	#[test]
	fn avc3_subsequent_frame_uses_cached_avcc() {
		let sps = &[0x67, 0x42, 0xc0, 0x1f, 0xde][..];
		let pps = &[0x68, 0xce, 0x3c, 0x80][..];
		let idr = &[0x65, 0x88][..];
		let p = &[0x61, 0xe0, 0x12][..];

		let mut tx = Avc1::new(None);
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

		let mut tx = Avc1::new(None);
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
