//! H.265 Annex-B stream splitter.
//!
//! The H.265 analogue of [`crate::codec::h264::Split`]: turns a raw Annex-B byte
//! stream (inline VPS/SPS/PPS) into [`crate::container::Frame`]s. It finds
//! access-unit boundaries, caches VPS/SPS/PPS and re-inserts them ahead of each
//! keyframe so every keyframe is self-contained, and stamps wall-clock
//! timestamps when the caller has none (stdin). It owns no track, catalog, or
//! codec config. The importer parses the codec config out of the frames it
//! emits.

use bytes::{Bytes, BytesMut};
use scuffle_h265::NALUnitType;

use super::Error;
use crate::Result;
use crate::codec::annexb::{NalIterator, START_CODE};

/// H.265 Annex-B stream splitter: bytes in, [`Frame`](crate::container::Frame)s out.
///
/// Feed bytes via [`decode`](Self::decode) (unknown frame boundaries, e.g.
/// stdin); call [`flush`](Self::flush) to emit the final in-flight access unit.
/// VPS/SPS/PPS seen inline are cached and re-inserted ahead of each keyframe so
/// each keyframe is self-contained.
pub struct Split {
	/// Bytes carried over between calls: complete NALs are parsed out on each
	/// [`decode`](Self::decode), leaving the in-flight (final, not-yet-terminated)
	/// NAL here until the next start code arrives or [`flush`](Self::flush) drains it.
	tail: BytesMut,
	current: Au,
	/// Retained VPS NALs from the latest keyframe that carried them, re-injected
	/// on bare keyframes. Replaced (not accumulated) when a keyframe presents a
	/// different set, so a mid-stream reinit drops the superseded ones.
	vps: Vec<Bytes>,
	/// Retained SPS NALs. See [`vps`](Self::vps).
	sps: Vec<Bytes>,
	/// Retained PPS NALs. A keyframe may carry several (slices reference them by
	/// id); all are kept and re-injected, but a new GOP's set supersedes them.
	pps: Vec<Bytes>,
	zero: Option<web_async::time::Instant>,
	pending: Vec<crate::container::Frame>,
}

#[derive(Default)]
struct Au {
	chunks: BytesMut,
	contains_idr: bool,
	contains_slice: bool,
	/// VPS NALs already inline in this access unit, so re-injection skips them.
	vps_seen: Vec<Bytes>,
	/// SPS NALs already inline in this access unit.
	sps_seen: Vec<Bytes>,
	/// PPS NALs already inline in this access unit.
	pps_seen: Vec<Bytes>,
}

impl Default for Split {
	fn default() -> Self {
		Self::new()
	}
}

impl Split {
	/// A fresh splitter with an empty parameter-set cache.
	pub fn new() -> Self {
		Self {
			tail: BytesMut::new(),
			current: Au::default(),
			vps: Vec::new(),
			sps: Vec::new(),
			pps: Vec::new(),
			zero: None,
			pending: Vec::new(),
		}
	}

	/// Decode a buffer where frame boundaries are unknown, returning the access
	/// units it can complete. The leading start code of the *next* access unit is
	/// what signals the previous one is complete, so the final NAL of the in-flight
	/// access unit stays buffered until the next call (or [`flush`](Self::flush)).
	/// The buffer is fully consumed.
	pub fn decode(
		&mut self,
		data: &[u8],
		pts: impl Into<Option<moq_net::Timestamp>>,
	) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts.into())?;
		self.tail.extend_from_slice(data);
		// Iterate complete NALs out of `tail`, leaving the trailing (in-flight) NAL
		// (with its start code) buffered for the next call or `flush`.
		let nals = NalIterator::new(&mut self.tail);
		let mut parsed = Vec::new();
		for nal in nals {
			parsed.push(nal?);
		}
		for nal in parsed {
			self.decode_nal(nal, pts)?;
		}
		Ok(std::mem::take(&mut self.pending))
	}

	/// Emit the in-flight access unit, if any. Call after the last
	/// [`decode`](Self::decode) when a caller handed over a complete access unit
	/// (or at end of stream) so the final NAL isn't left buffered.
	pub fn flush(&mut self, pts: impl Into<Option<moq_net::Timestamp>>) -> Result<Vec<crate::container::Frame>> {
		let pts = self.pts(pts.into())?;
		if let Some(nal) = NalIterator::new(&mut self.tail).flush()? {
			self.decode_nal(nal, pts)?;
		}
		self.tail.clear();
		self.maybe_start_frame(pts)?;
		Ok(std::mem::take(&mut self.pending))
	}

	/// Decode a single NAL unit. Only reads the first header byte to extract
	/// nal_unit_type, ignoring nuh_layer_id and nuh_temporal_id_plus1.
	fn decode_nal(&mut self, nal: Bytes, pts: moq_net::Timestamp) -> Result<()> {
		if nal.len() < 2 {
			return Err(Error::NalTooShort.into());
		}
		// u16 header: [forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id(6) | nuh_temporal_id_plus1(3)]
		let header = nal.first().ok_or(Error::NalTooShort)?;
		if (header >> 7) & 1 != 0 {
			return Err(Error::ForbiddenZeroBit.into());
		}

		let nal_type = nal_unit_type(*header);

		match nal_type {
			NALUnitType::VpsNut => {
				self.maybe_start_frame(pts)?;
				crate::codec::annexb::push_distinct(&mut self.current.vps_seen, &nal);
			}
			NALUnitType::SpsNut => {
				self.maybe_start_frame(pts)?;
				crate::codec::annexb::push_distinct(&mut self.current.sps_seen, &nal);
			}
			NALUnitType::PpsNut => {
				self.maybe_start_frame(pts)?;
				crate::codec::annexb::push_distinct(&mut self.current.pps_seen, &nal);
			}
			// Access-unit openers: each precedes the next picture's VCL data, so any
			// unit still open belongs to the previous picture and is emitted here.
			NALUnitType::AudNut | NALUnitType::PrefixSeiNut => {
				self.maybe_start_frame(pts)?;
			}
			// Suffix SEI trails the picture it describes, so it stays on the open
			// access unit. Closing here would hand it to the next picture, and at end
			// of stream would drop it: `flush` closes the unit it was moved out of,
			// then finds no slice in the one holding it.
			NALUnitType::SuffixSeiNut => {}
			// Keyframe containing slices.
			nal_type if super::is_irap(nal_type) => {
				// first_slice_segment_in_pic_flag (bit 7 of the third byte, after the
				// 2-byte header) marks the first slice of a new picture: close any access
				// unit still open. A bare IDR arriving right after a delta picture in the
				// same chunk would otherwise fold both into one frame and mis-flag it a keyframe.
				if nal.get(2).ok_or(Error::NalTooShort)? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				// Adopt this keyframe's inline set (dropping any the new GOP no longer
				// uses), or re-inject the retained set if the keyframe carried none.
				crate::codec::annexb::reconcile_keyframe_params(
					&mut self.current.chunks,
					&mut self.vps,
					&mut self.current.vps_seen,
				);
				crate::codec::annexb::reconcile_keyframe_params(
					&mut self.current.chunks,
					&mut self.sps,
					&mut self.current.sps_seen,
				);
				crate::codec::annexb::reconcile_keyframe_params(
					&mut self.current.chunks,
					&mut self.pps,
					&mut self.current.pps_seen,
				);

				self.current.contains_idr = true;
				self.current.contains_slice = true;
			}
			// All other slice types (both N and R variants).
			NALUnitType::TrailN
			| NALUnitType::TrailR
			| NALUnitType::TsaN
			| NALUnitType::TsaR
			| NALUnitType::StsaN
			| NALUnitType::StsaR
			| NALUnitType::RadlN
			| NALUnitType::RadlR
			| NALUnitType::RaslN
			| NALUnitType::RaslR => {
				// Check first_slice_segment_in_pic_flag (bit 7 of third byte, after 2-byte header).
				if nal.get(2).ok_or(Error::NalTooShort)? & 0x80 != 0 {
					self.maybe_start_frame(pts)?;
				}
				self.current.contains_slice = true;
			}
			_ => {}
		}

		// Replace the original start code with a canonical 4-byte start code (marginally
		// easier for downstream players, e.g. MSE).
		self.current.chunks.extend_from_slice(&START_CODE);
		self.current.chunks.extend_from_slice(&nal);

		Ok(())
	}

	fn maybe_start_frame(&mut self, pts: moq_net::Timestamp) -> Result<()> {
		if !self.current.contains_slice {
			return Ok(());
		}

		let payload = std::mem::take(&mut self.current.chunks).freeze();
		let keyframe = self.current.contains_idr;
		self.current.contains_idr = false;
		self.current.contains_slice = false;
		self.current.vps_seen.clear();
		self.current.sps_seen.clear();
		self.current.pps_seen.clear();

		self.pending.push(crate::container::Frame {
			timestamp: pts,
			payload,
			keyframe,
			duration: None,
		});
		Ok(())
	}

	/// Drop any in-flight access unit.
	///
	/// Pre-reset NALs would otherwise leak into a later frame with the wrong
	/// timestamp. The parameter-set cache is kept so subsequent keyframes stay
	/// self-contained.
	pub fn reset(&mut self) {
		self.current = Au::default();
		self.tail.clear();
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}
		let zero = self.zero.get_or_insert_with(web_async::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

/// Extract the HEVC `nal_unit_type` from the first header byte (bits 1..=6).
pub(super) fn nal_unit_type(header: u8) -> NALUnitType {
	NALUnitType::from((header >> 1) & 0b111111)
}

#[cfg(test)]
mod tests {
	use super::*;

	const SC4: &[u8] = &[0, 0, 0, 1];

	// HEVC NAL headers: byte0 = nal_unit_type << 1 (forbidden bit 0, layer id 0).
	const VPS: &[u8] = &[0x40, 0x01, 0x0c]; // type 32
	const SPS: &[u8] = &[0x42, 0x01, 0x01]; // type 33
	const PPS: &[u8] = &[0x44, 0x01, 0xc0]; // type 34
	const IDR: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // type 19 (IdrWRadl)
	// TrailR (type 1) with first_slice_segment_in_pic_flag set (byte 2 high bit).
	const TRAIL: &[u8] = &[0x02, 0x01, 0x80, 0x33];
	const AUD: &[u8] = &[0x46, 0x01, 0x50]; // type 35
	const PREFIX_SEI: &[u8] = &[0x4e, 0x01, 0x01, 0x04, 0x80]; // type 39
	const SUFFIX_SEI: &[u8] = &[0x50, 0x01, 0x84, 0x02, 0x80]; // type 40
	const SUFFIX_SEI2: &[u8] = &[0x50, 0x01, 0x05, 0x03, 0x80]; // a second type 40

	fn annexb(nals: &[&[u8]]) -> BytesMut {
		let mut buf = BytesMut::new();
		for nal in nals {
			buf.extend_from_slice(SC4);
			buf.extend_from_slice(nal);
		}
		buf
	}

	fn ts() -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(0).unwrap()
	}

	fn contains(haystack: &[u8], needle: &[u8]) -> bool {
		haystack.windows(needle.len()).any(|w| w == needle)
	}

	/// Decode one complete access unit handed over as a single buffer: `decode`
	/// buffers it, `flush` emits it.
	fn decode_one(split: &mut Split, buf: &mut BytesMut, pts: moq_net::Timestamp) -> Vec<crate::container::Frame> {
		let mut frames = split.decode(buf, pts).unwrap();
		frames.extend(split.flush(pts).unwrap());
		frames
	}

	/// Length-prefix each NAL with a 4-byte big-endian length: the hvc1 wire shape.
	fn length_prefixed(nals: &[&[u8]]) -> Bytes {
		let mut buf = BytesMut::new();
		for nal in nals {
			buf.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			buf.extend_from_slice(nal);
		}
		buf.freeze()
	}

	/// True for the NAL types [`Hvc1`](crate::codec::h265::Hvc1) lifts out of the
	/// sample and into the hvcC.
	fn is_param_set(nal: &[u8]) -> bool {
		matches!(
			nal_unit_type(nal[0]),
			NALUnitType::VpsNut | NALUnitType::SpsNut | NALUnitType::PpsNut
		)
	}

	/// One suffix-SEI case: name, the NALs fed to the splitter, and the access
	/// units it must emit.
	type SeiCase = (&'static str, Vec<&'static [u8]>, Vec<Vec<&'static [u8]>>);

	/// The access-unit shapes suffix SEI ownership has to get right.
	///
	/// Real x265 parameter sets, because the length-prefixed half of these cases
	/// runs the same access units through [`Hvc1`](crate::codec::h265::Hvc1),
	/// which parses the SPS to build the hvcC.
	fn suffix_sei_cases() -> Vec<SeiCase> {
		use crate::codec::h265::fixtures::{PPS, SPS, VPS};

		vec![
			(
				"consecutive pictures",
				vec![VPS, SPS, PPS, IDR, SUFFIX_SEI, AUD, TRAIL, SUFFIX_SEI],
				vec![vec![VPS, SPS, PPS, IDR, SUFFIX_SEI], vec![AUD, TRAIL, SUFFIX_SEI]],
			),
			(
				"multiple suffix units",
				vec![VPS, SPS, PPS, IDR, SUFFIX_SEI, SUFFIX_SEI2, AUD, TRAIL],
				vec![vec![VPS, SPS, PPS, IDR, SUFFIX_SEI, SUFFIX_SEI2], vec![AUD, TRAIL]],
			),
			(
				"suffix at EOF",
				vec![VPS, SPS, PPS, IDR, AUD, TRAIL, SUFFIX_SEI],
				vec![vec![VPS, SPS, PPS, IDR], vec![AUD, TRAIL, SUFFIX_SEI]],
			),
			(
				"prefix immediately after suffix",
				vec![VPS, SPS, PPS, IDR, SUFFIX_SEI, PREFIX_SEI, TRAIL],
				vec![vec![VPS, SPS, PPS, IDR, SUFFIX_SEI], vec![PREFIX_SEI, TRAIL]],
			),
		]
	}

	/// Annex-B: a suffix SEI opens nothing, so it stays on the picture it follows
	/// rather than moving to the next one. The EOF case is the sharp one: the
	/// stream's last NAL is a suffix SEI, and closing the access unit on it would
	/// leave `flush` with nothing to emit it in.
	#[tokio::test(start_paused = true)]
	async fn annexb_suffix_sei_stays_with_its_picture() {
		for (name, stream, expected) in suffix_sei_cases() {
			let mut split = Split::new();
			let frames = decode_one(&mut split, &mut annexb(&stream), ts());

			assert_eq!(frames.len(), expected.len(), "{name}: access unit count");
			for (i, (frame, nals)) in frames.iter().zip(&expected).enumerate() {
				assert_eq!(
					frame.payload.as_ref(),
					annexb(nals).freeze().as_ref(),
					"{name}: access unit {i}"
				);
			}
			assert!(frames[0].keyframe, "{name}: the first access unit is the keyframe");
			assert!(!frames[1].keyframe, "{name}: the second is a delta picture");
		}
	}

	/// hvc1: the same ownership survives the length-prefixed wire shape.
	/// [`Hvc1`](crate::codec::h265::Hvc1) lifts VPS/SPS/PPS into the hvcC, so each
	/// sample is the picture's own NALs, suffix SEI included and in order.
	#[tokio::test(start_paused = true)]
	async fn length_prefixed_suffix_sei_stays_with_its_picture() {
		for (name, stream, expected) in suffix_sei_cases() {
			let mut split = Split::new();
			let frames = decode_one(&mut split, &mut annexb(&stream), ts());
			let mut hvc1 = crate::codec::h265::Hvc1::new();

			assert_eq!(frames.len(), expected.len(), "{name}: access unit count");
			for (i, (frame, nals)) in frames.iter().zip(&expected).enumerate() {
				let sample = hvc1
					.transform(frame.payload.clone())
					.unwrap()
					.expect("an access unit with slice data");
				let want: Vec<&[u8]> = nals.iter().copied().filter(|nal| !is_param_set(nal)).collect();
				assert_eq!(sample.as_ref(), length_prefixed(&want).as_ref(), "{name}: sample {i}");
			}
			assert!(
				hvc1.hvcc().is_some(),
				"{name}: the keyframe's parameter sets build an hvcC"
			);
		}
	}

	/// A keyframe access unit fed as one buffer emits one self-contained frame:
	/// VPS+SPS+PPS are packaged ahead of the IDR slice and `keyframe` is set.
	#[tokio::test(start_paused = true)]
	async fn decode_packages_keyframe() {
		let mut split = Split::new();
		let frames = decode_one(&mut split, &mut annexb(&[VPS, SPS, PPS, IDR]), ts());

		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert!(contains(&frames[0].payload, VPS));
		assert!(contains(&frames[0].payload, SPS));
		assert!(contains(&frames[0].payload, PPS));
		assert!(contains(&frames[0].payload, IDR));
	}

	/// Parameter sets fed up front (as the leading stream bytes) are cached and
	/// re-inserted ahead of a later bare IDR, so the keyframe is self-contained
	/// even when the stream never repeats its parameter sets inline.
	#[tokio::test(start_paused = true)]
	async fn params_then_bare_keyframe_self_contained() {
		let mut split = Split::new();
		// The leading VPS/SPS/PPS carry no slice, so they complete no frame yet.
		assert!(split.decode(&annexb(&[VPS, SPS, PPS]), ts()).unwrap().is_empty());

		let frames = decode_one(&mut split, &mut annexb(&[IDR]), ts());
		assert_eq!(frames.len(), 1);
		assert!(frames[0].keyframe);
		assert!(contains(&frames[0].payload, VPS));
		assert!(contains(&frames[0].payload, SPS));
		assert!(contains(&frames[0].payload, PPS));
	}

	/// A source that defines two PPS (and is otherwise normal) once, then sends a
	/// bare IDR: both cached PPS must be re-injected on the keyframe, not just the
	/// last one. Regression for the multi-PPS collapse.
	#[tokio::test(start_paused = true)]
	async fn reinjects_all_cached_pps_on_keyframe() {
		const PPS1: &[u8] = &[0x44, 0x01, 0xc1]; // second PPS, type 34

		let mut split = Split::new();
		let first = decode_one(&mut split, &mut annexb(&[VPS, SPS, PPS, PPS1, IDR]), ts());
		assert_eq!(first.len(), 1);
		assert!(first[0].keyframe);

		// Bare IDR: the splitter re-injects VPS + SPS + both PPS in order.
		let second = decode_one(&mut split, &mut annexb(&[IDR]), ts());
		assert_eq!(second.len(), 1);
		assert!(second[0].keyframe);
		assert_eq!(
			second[0].payload.as_ref(),
			annexb(&[VPS, SPS, PPS, PPS1, IDR]).freeze().as_ref()
		);
	}

	/// A bare IDR arriving right after a delta picture in the same decode chunk must
	/// open its own access unit, not fold into the delta's frame. Without closing the
	/// open slice on the IDR's first slice, the two AUs merge and the result is
	/// mis-flagged as a keyframe.
	#[tokio::test(start_paused = true)]
	async fn bare_idr_after_delta_splits() {
		let mut split = Split::new();
		// One chunk: keyframe, a delta picture, then a bare IDR (no inline params).
		let frames = split
			.decode(&annexb(&[VPS, SPS, PPS, IDR, TRAIL, IDR, AUD]), ts())
			.unwrap();

		assert_eq!(frames.len(), 2);
		assert!(frames[0].keyframe, "first AU is the keyframe");
		assert!(!frames[1].keyframe, "the delta picture must not be flagged a keyframe");
		assert_eq!(frames[1].payload.as_ref(), annexb(&[TRAIL]).freeze().as_ref());

		// Flushing closes the bare IDR as its own self-contained keyframe.
		let tail = split.flush(ts()).unwrap();
		assert_eq!(tail.len(), 1);
		assert!(tail[0].keyframe);
		assert_eq!(
			tail[0].payload.as_ref(),
			annexb(&[VPS, SPS, PPS, IDR]).freeze().as_ref()
		);
	}

	/// A keyframe that presents a smaller parameter set than a prior one reinits
	/// the retained set: the dropped PPS must not be re-injected on later bare
	/// keyframes.
	#[tokio::test(start_paused = true)]
	async fn reinit_drops_superseded_pps_on_keyframe() {
		const PPS1: &[u8] = &[0x44, 0x01, 0xc1];

		let mut split = Split::new();
		let _ = decode_one(&mut split, &mut annexb(&[VPS, SPS, PPS, PPS1, IDR]), ts());
		let _ = decode_one(&mut split, &mut annexb(&[VPS, SPS, PPS, IDR]), ts());
		let third = decode_one(&mut split, &mut annexb(&[IDR]), ts());

		assert_eq!(third.len(), 1);
		assert!(third[0].keyframe);
		assert_eq!(
			third[0].payload.as_ref(),
			annexb(&[VPS, SPS, PPS, IDR]).freeze().as_ref()
		);
	}
}
